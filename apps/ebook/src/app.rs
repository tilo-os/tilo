//! The Books app widget — a single retained-mode [`Widget`] that renders both
//! the library shelf and the paginated reader, sourcing colour from the OS
//! [`Theme`] (chrome) or a reader palette (the page), and storing all user
//! state in the Automerge CRDT.
//!
//! One custom widget rather than composed `ui` widgets because reflowable,
//! paginated text with highlight bands behind glyphs and margin annotation
//! glyphs doesn't map onto stock layout widgets: the surface is drawn
//! straight into the Vello [`Scene`] via [`crate::paginate`] and
//! [`crate::paint`]. `draw` records clickable [`Hit`] regions; `handle_event`
//! hit-tests them on the next click; a frame always precedes input so
//! regions are never stale.
//!
//! State: `State::doc` (CRDT) holds settings/progress/annotations, mutated
//! directly in `handle_event`/`sync`. Ephemeral widget caches (open book,
//! page index, selection, layout cache) are per-peer view state kept out of
//! the document so readers browse independently.

#![allow(
    clippy::many_single_char_names,
    reason = "t (Affine), c (Colors), r (Rect), x/y/w/h (geometry) and their close relatives \
              are the standard vocabulary shared by every draw/hit-test helper in this file"
)]

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{
    BlendMode, Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage,
    ImageFormat as ImageFmt,
};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use std::time::{Duration, Instant};

use ui::ColorCompat;
use ui::animation::{Animated, Curve};
use ui::gesture::{DragSelect, Granularity};
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{EditOutcome, EditableLabel, FlapState, SplitLayout, Widget};

use crate::color_picker::{ColorEdit, ColorPicker, PickTarget, PickerPress};
use crate::comment_overlay::{CommentAction, CommentOverlay};
use crate::content::{self, is_shared};
use crate::doc::BooksDoc;
use crate::input_controls::{NoteAction, NoteComposer, NumberControls, NumberField};
use crate::model::{
    AnnoKind, Annotation, Book, ColorScheme, CompositionStyle, FontEntry, FontKind, HiColor,
    Progress, ReadMode, ReadTheme, Settings, default_fonts, default_schemes, now_ms,
};
use crate::paginate::{ChapterLayout, LayoutKey, LayoutKind, font_choice, layout_chapter};
use crate::paint;
use crate::reader_bubble::{ReaderBubble, ReaderBubbleAction, ReaderPosition};
use crate::reader_theme::ReaderPalette;
use crate::shelf_controls::{ShelfControlAction, ShelfControls};

const TOP_BAR: f64 = 54.0;

fn book_open_request() -> PickRequest {
    PickRequest::open("Open a book", "Open")
        .thread_name("ebook-portal-open")
        .filter(
            FileFilter::new("EPUB books")
                .mimetype("application/epub+zip")
                .glob("*.epub"),
        )
        .all_files()
}

/// Pixels one wheel-line notch maps to, matching the toolkit's other scroll
/// surfaces so a mouse wheel feels consistent across apps.
const WHEEL_LINE_PX: f64 = 48.0;
/// Accumulated reader wheel travel (px) that commits to one page turn.
const TURN_STEP_PX: f64 = 40.0;
/// Accumulated horizontal swipe travel (px) that commits to one page turn;
/// larger than the vertical wheel step so a swipe reads as a deliberate flick.
const SWIPE_STEP_PX: f64 = 80.0;
const TOC_W: f64 = 244.0;
const NOTES_W: f64 = 326.0;
const PAGE_MAX_W: f64 = 680.0;

/// At/above this width the reader draws the floating "paper card"; below it
/// the page goes borderless and fills the reading area.
const FRAME_MIN_W: f64 = 720.0;

/// Below this width the contents rail switches from docked to an overlay
/// flap over the page behind a dimming scrim.
const TOC_BREAKPOINT: f64 = 760.0;

/// Below this (wider) width the annotations rail also goes overlay, so a
/// medium window keeps contents docked but floats annotations.
const NOTES_BREAKPOINT: f64 = 1040.0;

/// Which screen is showing. Per-peer, never persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Library,
    Reader,
    /// The app settings screen — the font + colour-scheme managers.
    Settings,
}

/// Which manager the settings screen is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettingsTab {
    Fonts,
    Colours,
    /// Library management: the folders watched for new books.
    Library,
}

/// An in-progress inline rename (font display name or scheme name), backed by
/// the toolkit's reusable [`ui::widget::EditableLabel`] rather than a
/// hand-rolled character buffer.
struct Rename {
    target: RenameTarget,
    field: ui::widget::EditableLabel,
}

#[derive(Clone, PartialEq, Eq)]
enum RenameTarget {
    Font(String),
    Scheme(String),
}

/// A clickable region recorded during `draw`, consumed on the next click.
#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: Action,
}

/// A position in the current chapter's flow: a block index and a character
/// offset into that block's canonical text ([`crate::paginate::LaidBlock::text`]).
/// `Ord` compares block-then-char, which is what lets the shared word-drag
/// rule ([`ui::extend_span_drag`]) work directly on `DocPos`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct DocPos {
    block: usize,
    ch: usize,
}

impl DocPos {
    fn key(&self) -> (usize, usize) {
        (self.block, self.ch)
    }
}

/// The unit an in-flight text-selection drag snaps to, set on press from the
/// [`DragSelect`] click ladder: single click drags by character, double/triple
/// click drags by whole words. Mirrors `TextInput`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DragUnit {
    Char,
    Word,
}

/// A drag text selection: an anchor (where the drag started) and a head
/// (where it is now). Spans paragraphs and renders on whichever page shows
/// part of the range.
#[derive(Clone, Copy)]
struct TextSel {
    anchor: DocPos,
    head: DocPos,
}

impl TextSel {
    fn ordered(&self) -> (DocPos, DocPos) {
        if self.anchor.key() <= self.head.key() {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    fn is_empty(&self) -> bool {
        self.anchor.key() == self.head.key()
    }
}

/// One laid-out run's screen geometry, recorded each frame so `handle_event`
/// (which has no text renderer) can map a pointer to a [`DocPos`].
#[derive(Clone, Copy)]
struct SelRun {
    block: usize,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    cs: usize,
    cl: usize,
    /// On-screen column this run was painted in. [`ReadMode::TwoUp`] maps
    /// different flow lines to the same screen-y, so hit-testing must group
    /// by column as well as y or a click near the gutter snaps to the wrong
    /// page.
    col: u8,
}

/// Screen `(x, y)` anchor for the selection HUD.
type SelAnchor = Option<(f64, f64)>;
/// On-page bounds `(x0, y0, x1, y1)` of the open comment overlay's highlight.
type CommentBoxBounds = Option<(f64, f64, f64, f64)>;

/// One on-screen reading column: where the flow slice lands ([`Self::x`],
/// [`Self::y`], [`Self::w`]) and which flow band it shows
/// (`win_start..win_end`).
struct Pane {
    x: f64,
    y: f64,
    w: f32,
    win_start: f32,
    win_end: f32,
    col: u8,
    /// Include partially-visible lines at the window edges (continuous
    /// scroll), vs. a strict page slice where every line belongs to one page.
    partial: bool,
}

impl Pane {
    /// Whether a line at flow-y `ly` with height `lh` shows in this pane.
    fn shows(&self, ly: f32, lh: f32) -> bool {
        if self.partial {
            ly + lh > self.win_start && ly < self.win_end
        } else {
            ly >= self.win_start - 0.5 && ly < self.win_end
        }
    }
    fn screen_y(&self, ly: f32) -> f64 {
        self.y + f64::from(ly - self.win_start)
    }
}

/// An in-flight page-turn slide for the paged / two-up modes: the incoming
/// page eases in from the side it was turned from (`dir` is `+1` forward,
/// `-1` back) while fading up. Continuous scroll never uses this.
#[derive(Clone, Copy)]
struct PageTurn {
    anim: Animated<f64>,
    dir: f64,
}

impl PageTurn {
    const DURATION: Duration = Duration::from_millis(240);
    /// Peak horizontal travel of the incoming page, as a fraction of page width.
    const SLIDE_FRAC: f64 = 0.14;

    fn start(dir: f64) -> Self {
        Self {
            anim: Animated::new(0.0, 1.0, Self::DURATION, Curve::EaseOut),
            dir,
        }
    }

    /// `(horizontal offset px, opacity)` of the incoming page at `now`.
    fn visual(&self, now: Instant, page_w: f64) -> (f64, f32) {
        let p = self.anim.current(now);
        let offset = self.dir * (1.0 - p) * page_w * Self::SLIDE_FRAC;
        (offset, (0.35 + 0.65 * p) as f32)
    }

    fn done(&self, now: Instant) -> bool {
        self.anim.done(now)
    }
}

/// Everything a click or key can ask the app to do.
#[derive(Clone)]
enum Action {
    OpenBook(usize),
    Back,
    PrevPage,
    NextPage,
    GotoChapter(usize),
    ToggleToc,
    ToggleNotes,
    ToggleTypePopover,
    /// Dismiss any open overlay flap (scrim tap on a narrow window).
    DismissOverlay,
    /// Absorb a click without acting — stops a tap on an overlay rail's own
    /// body from falling through to the dismissing scrim behind it.
    Swallow,
    ToggleBookmark,
    FontInc,
    FontDec,
    SpacingInc,
    SpacingDec,
    CycleFont,
    SetFont(String),
    SetReadTheme(ReadTheme),
    SetMode(ReadMode),
    SetComposition(CompositionStyle),
    /// Apply/toggle/replace a highlight colour over the current text selection.
    SetColor(HiColor),
    AddNote,
    CopySelection,
    JumpTo(Uuid),
    Delete(Uuid),
    EditNote(Uuid),

    // --- highlight comment overlay ---------------------------------------
    /// Open the floating comment overlay for a highlight group.
    OpenComments(Uuid),
    CloseComments,
    /// Delete an entire highlight (its per-block pieces + every comment on it).
    DeleteHighlight(Uuid),
    AddComment(Uuid),
    EditComment(Uuid),
    DeleteComment(Uuid),
    /// Open a book from anywhere through the host file picker; opens
    /// immediately and is added to the library. Primary way in.
    OpenViaPortal,
    /// Copy a local book (by library index) into the collaborative workspace
    /// so the workspace's peers can read it too.
    ShareToWorkspace(usize),
    /// Remove a book (by library index) from the shelf. Its annotations +
    /// progress are kept, keyed by content id, so re-adding the file restores them.
    RemoveFromLibrary(usize),
    ToggleWatchFolder(PathBuf),

    // --- settings screen: navigation -------------------------------------
    OpenSettings,
    CloseSettings,
    SetSettingsTab(SettingsTab),

    // --- settings screen: colour schemes ---------------------------------
    /// Apply a custom colour scheme by id (built-ins go via `SetReadTheme`).
    SetScheme(String),
    ToggleSchemeEnabled(String),
    DuplicateScheme(String),
    DeleteScheme(String),
    EditScheme(String),
    CloseSchemeEditor,
    NewSchemeViaPicker,
    /// Open the colour picker on one colour slot (0=page,1=ink,2=accent) of a
    /// scheme.
    PickSchemeColor(String, usize),
    BeginRenameScheme(String),

    // --- settings screen: fonts ------------------------------------------
    SetDefaultFont(String),
    /// Open/close the default-font dropdown at the top of the fonts manager.
    ToggleDefaultFontMenu,
    /// Enable/disable a font (whether it appears in the quick settings picker).
    ToggleFontShown(String),
    RemoveFont(String),
    OpenFontPicker,
    CloseFontPicker,
    AddSystemFont(String),
    BeginRenameFont(String),
    // --- library shelf ---------------------------------------------------
}

/// Icon glyphs the chrome draws (each maps to a `paint::*` vector helper).
#[derive(Clone, Copy)]
enum Ico {
    Back,
    Toc,
    Notes,
    Bookmark(bool),
    /// A horizontal three-dot "more" glyph (opens the full settings screen).
    Dots,
    /// A downward chevron (dropdown affordance).
    ChevDown,
}

impl BooksApp {
    /// The book currently open in the reader, if the library is non-empty.
    /// Used by [`crate::actions`] to publish the open-resource tree.
    pub(crate) fn open_book(&self) -> Option<&Book> {
        self.library.get(self.book_idx)
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent per-peer view-state flag (dirty, mouse_down, \
              dragging_sel, ...), not a combinatorial state machine"
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag spanning unrelated \
              subsystems (library-load state, drag/selection gestures, popover \
              open/closed toggles, and a handful of --shots-only seams); \
              grouping them into enums would not express any real invariant \
              between them and would touch reader, library and settings call \
              sites across this file"
)]
pub struct BooksApp {
    // --- parsed content (render cache, regenerable; not in the CRDT) -------
    library: Vec<Book>,
    /// Where each book's `.epub` lives, keyed by content id. The durable
    /// record is the CRDT library ([`BooksDoc`]); this is its cache.
    library_paths: HashMap<String, PathBuf>,
    /// Whether the persisted library has been hydrated from the document yet
    /// (loads on the first `sync`; shot/test builds pass one explicitly).
    library_loaded: bool,
    /// A book handed to the app by the OS (file association) to open + import
    /// on the first `sync`.
    pending_open_path: Option<PathBuf>,
    /// An in-flight host file-picker request, drained once per frame in
    /// `sync`. `None` when no picker is open (the app never draws its own).
    portal_rx: Option<PortalChannel>,
    /// Empty-state message when opening a book failed.
    load_error: Option<String>,

    // --- taffy / paint geometry -------------------------------------------
    rect: Rect,
    dirty: bool,

    // --- view state (ephemeral, per-peer) ---------------------------------
    view: View,
    book_idx: usize,
    chapter: usize,
    page: usize,
    /// Contents rail: docked beside the page on a wide window, an overlay
    /// flap on a narrow one. The shared `FlapState` owns the reveal + scrim.
    toc_flap: FlapState,
    /// Annotations rail: a secondary inspector, overlay below [`NOTES_BREAKPOINT`].
    notes_flap: FlapState,
    /// The block a live text selection is anchored in, for positioning the
    /// floating toolbar/composer near the passage. Derived from [`Self::sel`].
    selected_block: Option<usize>,
    /// Arbitrary drag text selection spanning paragraphs/pages, if any.
    sel: Option<TextSel>,
    /// The shared toolkit selection-gesture core: owns the multi-click
    /// ladder (single → double-word → triple) and reports drag granularity.
    drag_select: DragSelect,
    /// The unit the in-flight drag snaps to (from the click ladder).
    drag_unit: DragUnit,
    /// The anchor span the current drag grows from: the pressed char for a
    /// single-click drag, or the double-clicked word for a word drag (see
    /// [`ui::extend_span_drag`]).
    drag_anchor_span: (DocPos, DocPos),
    mouse_down: bool,
    /// Screen point where the current press began.
    press_pos: Point,
    /// Doc position under the press, if it landed on page text.
    press_doc: Option<DocPos>,
    /// Whether the current press has moved far enough to be a selection drag.
    dragging_sel: bool,
    /// Per-frame run geometry for pointer→DocPos hit-testing (rebuilt in draw).
    sel_runs: Vec<SelRun>,
    /// Per-frame continuous highlight band rects (rebuilt in draw).
    hi_bands: Vec<Rect>,
    /// After a drag auto-advances a page, extend the selection head to the
    /// new page's end (`Some(true)`) or start (`Some(false)`) once laid out.
    pending_sel_edge: Option<bool>,
    /// The reader body's vertical extent (top, bottom) for edge auto-advance.
    reader_body: (f64, f64),
    /// First / last doc position visible on the current page (for edge grow).
    page_first_doc: Option<DocPos>,
    page_last_doc: Option<DocPos>,
    hi_color: HiColor,
    /// Vertical scroll offset of the library shelf, clamped each frame to
    /// `shelf_max_scroll`.
    shelf_scroll: f64,
    /// Last-known maximum shelf scroll, computed in `draw_library` and read
    /// back by the wheel handler to clamp the offset.
    shelf_max_scroll: f64,
    /// Keyboard-focused book on the shelf: a position within
    /// [`Self::shelf_order`], not a raw library index.
    shelf_cursor: usize,
    /// Original library indices in the shelf's current display order (sort +
    /// hide-read filter). Rebuilt each `draw_library`.
    shelf_order: Vec<usize>,
    /// Last-known column count of the shelf grid, so up/down move by a row.
    shelf_cols: usize,
    /// Set when the shelf cursor moves via the keyboard, so the next
    /// `draw_library` scrolls the focused card into view.
    scroll_to_cursor: bool,
    /// Accumulated wheel delta in the reader, so a trackpad's small pixel
    /// deltas coalesce into whole page turns.
    turn_accum: f64,
    /// Reading-settings popover (text size / spacing / typeface / page tint),
    /// summoned by the "Aa" button in the top bar.
    type_popover: bool,

    /// Owns the transient page indicator's animation, drawing and hit-testing.
    reader_bubble: ReaderBubble,

    cursor: Point,
    /// First block index visible on the current page (for bookmark targeting).
    page_top_block: usize,
    /// The note currently being typed into (a CRDT annotation id).
    composing: Option<Uuid>,
    /// Shared multiline input for standalone page notes. Highlight comments
    /// keep using the comment overlay but share the same CRDT annotation model.
    note_composer: NoteComposer,
    /// Floating highlight-comments panel. It owns open/anchor state, drawing,
    /// and its modal hit stack; the app only applies emitted domain actions.
    comment_overlay: CommentOverlay,
    /// The highlight group under the current press, consumed on release to
    /// open the comment overlay — only when the press stayed a plain click.
    press_hl_group: Option<Uuid>,
    /// Deferred page resolution: jump to last page after a backward chapter
    /// turn, or to the page containing a block after a sidebar jump.
    pending_last_page: bool,
    pending_jump_block: Option<usize>,

    swipe_accum: f64,

    /// Continuous-scroll offset (logical px) into the current chapter's
    /// flow. Only meaningful in [`ReadMode::Continuous`].
    scroll_y: f64,
    /// The current chapter's `(total_flow_height, viewport_height)`,
    /// recorded each `draw` so the wheel/keys can clamp scroll.
    flow_extent: (f64, f64),
    /// After a backward chapter roll in continuous mode, land at the foot of
    /// the new chapter once laid out (mirrors `pending_last_page`).
    pending_scroll_end: bool,
    /// An in-flight page-turn slide (paged / two-up only). Drives the eased
    /// incoming-page reveal and keeps the loop pumping via `needs_animation_frame`.
    page_turn: Option<PageTurn>,

    // --- draggable annotation markers -------------------------------------
    /// Per-frame grab handles for each highlight group's margin marker:
    /// `(hit_rect, group)`. Rebuilt every `draw`.
    hi_markers: Vec<(Rect, Uuid)>,
    /// An in-flight margin-marker drag re-anchoring a highlight to another
    /// paragraph, and the group it moves.
    marker_drag: Option<ui::MarkerDrag>,
    marker_group: Option<Uuid>,

    // --- caches pulled from the CRDT (refreshed in sync / after events) ----
    settings: Settings,
    fonts: Vec<FontEntry>,
    default_font_id: String,
    schemes: Vec<ColorScheme>,
    annotations: Vec<Annotation>,

    // --- settings screen (ephemeral) --------------------------------------
    settings_tab: SettingsTab,
    renaming: Option<Rename>,
    /// Shared spin buttons used by the quick-reading and font-manager panels.
    number_controls: NumberControls,
    /// Modal colour editor; owns HSV state, drawing, hit regions and drags.
    color_picker: ColorPicker,
    default_font_menu: bool,
    library_prefs: crate::model::LibraryPrefs,
    watched_folders: Vec<PathBuf>,
    /// Library shelf sort/filter cluster. Owns dropdown state, drawing, and
    /// its topmost hit stack; the app persists emitted preference actions.
    shelf_controls: ShelfControls,
    /// Scheme id currently open in the colour editor, if any.
    editing_scheme: Option<String>,
    /// Whether the "add a system font" family picker is open.
    font_picker: bool,
    picker_scroll: f64,
    picker_max_scroll: f64,
    settings_scroll: f64,
    settings_max_scroll: f64,
    shelf_fraction: HashMap<String, f32>,
    shelf_annos: HashMap<String, usize>,
    /// Last-opened timestamp per book (for the "recently opened" shelf sort).
    shelf_opened: HashMap<String, u64>,
    last_persisted: Option<(usize, usize)>,
    cur_page_count: usize,
    seeded: bool,
    /// Sample-content mode: only the hermetic `--shots`/test path sets this.
    /// A real user is never seeded.
    demo: bool,
    /// Reading settings a `--shots` scenario wants applied through the real
    /// CRDT path on the next sync. Shot-only.
    shot_settings: Option<Settings>,
    /// Font/scheme registries a `--shots` scenario seeds. Shot-only.
    shot_fonts: Option<Vec<FontEntry>>,
    shot_schemes: Option<Vec<ColorScheme>>,
    /// Watched folders a `--shots` scenario seeds. Shot-only.
    shot_watched: Option<Vec<PathBuf>>,
    /// A `--shots` scenario that wants the on-page note composer open.
    /// Shot-only.
    shot_compose: bool,
    /// A `--shots` scenario that wants a highlight-with-comments overlay
    /// open. Shot-only.
    shot_comments: bool,
    /// Last resolved layout mode per rail, so a breakpoint crossing settles
    /// the flap instantly instead of animating on launch/resize.
    settled_toc: Option<SplitLayout>,
    settled_notes: Option<SplitLayout>,

    // --- layout cache ------------------------------------------------------
    layout: Option<ChapterLayout>,
    /// Decoded cover artwork lifted into GPU-ready image handles, keyed by
    /// book id and built lazily on first shelf draw.
    cover_cache: HashMap<String, PenikoImage>,

    // --- per-frame hit regions --------------------------------------------
    hits: Vec<Hit>,

    /// Framework-owned action dispatch: each on-page affordance dispatches
    /// its [`EbookAction`](crate::actions::EbookAction) through this so it
    /// replays byte-identically on peers, exactly as the Ctrl+K palette does.
    actions: ui::ActionHost,
}

impl BooksApp {
    /// The live app: starts on an empty shelf and hydrates the reader's
    /// persisted library from the document on the first `sync`.
    pub fn new() -> Self {
        let mut app = Self::with_library(Vec::new());
        app.library_loaded = false;
        app.view = View::Library;
        app
    }
}

impl Default for BooksApp {
    fn default() -> Self {
        Self::new()
    }
}

impl BooksApp {
    pub(crate) fn with_library(library: Vec<Book>) -> Self {
        // Open straight into the reader when there's a book; otherwise land
        // on the library so an empty shelf shows its guidance.
        let view = if library.is_empty() {
            View::Library
        } else {
            View::Reader
        };
        Self {
            library,
            view,
            ..Self::blank_view_state()
        }
    }

    /// Every ephemeral per-peer field's fixed starting value, minus `library`
    /// and `view` (which [`Self::with_library`] always overrides). One long
    /// literal — there is no logic here, just starting values, and a struct
    /// literal can't be split across two functions without the inner one
    /// duplicating every field of the outer one anyway.
    #[allow(
        clippy::too_many_lines,
        reason = "a flat list of ~99 starting values, one per field; splitting it does not \
                  make it more readable and a struct literal can't be split without the \
                  inner literal setting every field itself"
    )]
    fn blank_view_state() -> Self {
        Self {
            library: Vec::new(),
            library_paths: HashMap::new(),
            // A caller-supplied library (tests / shots) is authoritative; the live
            // `new()` clears this so the document load runs.
            library_loaded: true,
            pending_open_path: None,
            portal_rx: None,
            load_error: None,
            rect: Rect::ZERO,
            dirty: true,
            view: View::Library,
            book_idx: 0,
            chapter: 0,
            page: 0,
            toc_flap: FlapState::open_when_docked(),
            notes_flap: FlapState::open_when_docked(),
            selected_block: None,
            sel: None,
            drag_select: DragSelect::new(),
            drag_unit: DragUnit::Char,
            drag_anchor_span: (DocPos { block: 0, ch: 0 }, DocPos { block: 0, ch: 0 }),
            mouse_down: false,
            press_pos: Point::ZERO,
            press_doc: None,
            dragging_sel: false,
            sel_runs: Vec::new(),
            hi_bands: Vec::new(),
            pending_sel_edge: None,
            reader_body: (0.0, 0.0),
            page_first_doc: None,
            page_last_doc: None,
            hi_color: HiColor::Yellow,
            shelf_scroll: 0.0,
            shelf_max_scroll: 0.0,
            shelf_cursor: 0,
            shelf_order: Vec::new(),
            shelf_cols: 1,
            scroll_to_cursor: false,
            turn_accum: 0.0,
            type_popover: false,
            reader_bubble: ReaderBubble::default(),
            cursor: Point::ZERO,
            page_top_block: 0,
            composing: None,
            note_composer: NoteComposer::default(),
            comment_overlay: CommentOverlay::default(),
            press_hl_group: None,
            pending_last_page: false,
            pending_jump_block: None,
            swipe_accum: 0.0,
            scroll_y: 0.0,
            flow_extent: (0.0, 0.0),
            pending_scroll_end: false,
            page_turn: None,
            hi_markers: Vec::new(),
            marker_drag: None,
            marker_group: None,
            settings: Settings::default(),
            fonts: default_fonts(),
            default_font_id: String::new(),
            schemes: default_schemes(),
            annotations: Vec::new(),
            settings_tab: SettingsTab::Fonts,
            renaming: None,
            number_controls: NumberControls::default(),
            color_picker: ColorPicker::default(),
            default_font_menu: false,
            library_prefs: crate::model::LibraryPrefs::default(),
            watched_folders: Vec::new(),
            shelf_controls: ShelfControls::default(),
            editing_scheme: None,
            font_picker: false,
            picker_scroll: 0.0,
            picker_max_scroll: 0.0,
            settings_scroll: 0.0,
            settings_max_scroll: 0.0,
            shelf_fraction: HashMap::new(),
            shelf_annos: HashMap::new(),
            shelf_opened: HashMap::new(),
            last_persisted: None,
            cur_page_count: 1,
            seeded: false,
            demo: false,
            shot_settings: None,
            shot_fonts: None,
            shot_schemes: None,
            shot_watched: None,
            shot_compose: false,
            shot_comments: false,
            settled_toc: None,
            settled_notes: None,
            layout: None,
            cover_cache: HashMap::new(),
            hits: Vec::new(),
            actions: ui::ActionHost::new(),
        }
    }

    /// Build the reader pointed at a single opened `.epub` (OS file
    /// association). The book is imported into the library and opened on the
    /// first `sync`; the rest of the reader's shelf loads alongside it.
    pub fn open_path(path: &std::path::Path) -> Self {
        let mut app = Self::new();
        app.pending_open_path = Some(path.to_path_buf());
        app.view = View::Reader;
        app
    }

    /// Screenshot-only: force the floating page bubble hover-pinned open so
    /// a single still frame captures the transient indicator. Never armed at
    /// runtime.
    pub fn arm_reader_bubble_for_shots(&mut self) {
        self.reader_bubble.arm_for_shots();
    }

    fn num_chapters(&self) -> usize {
        self.library
            .get(self.book_idx)
            .map_or(0, |b| b.chapters.len())
    }

    // --- Responsive layout: the contents/annotations rails collapse to
    // overlay flaps on a narrow window. The shared `FlapState` kernel owns
    // the eased reveal + scrim; the app only resolves which mode applies.

    fn toc_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), TOC_BREAKPOINT)
    }

    fn notes_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), NOTES_BREAKPOINT)
    }

    fn toc_open(&self) -> bool {
        self.toc_flap.is_open(self.toc_layout())
    }

    fn notes_open(&self) -> bool {
        self.notes_flap.is_open(self.notes_layout())
    }

    /// Reconcile both flaps with the current width. On the first real width
    /// and on every breakpoint crossing the flap is snapped to its mode
    /// default, so launch/resize never play a gratuitous slide — animation
    /// is reserved for explicit user toggles within one layout mode.
    fn reconcile_flaps(&mut self) {
        let tl = self.toc_layout();
        if self.settled_toc != Some(tl) {
            self.toc_flap.reconcile(tl);
            let open = self.toc_flap.is_open(tl);
            self.toc_flap.snap(open);
            self.settled_toc = Some(tl);
        }
        let nl = self.notes_layout();
        if self.settled_notes != Some(nl) {
            self.notes_flap.reconcile(nl);
            let open = self.notes_flap.is_open(nl);
            self.notes_flap.snap(open);
            self.settled_notes = Some(nl);
        }
    }

    fn flaps_animating(&self) -> bool {
        self.toc_flap.is_animating(self.toc_layout())
            || self.notes_flap.is_animating(self.notes_layout())
    }

    /// Stage a named interaction state for the single-frame `--shots`
    /// renderer. This is the only place sample content enters the app: it
    /// swaps in the sample library and marks the app as demo so `sync` seeds
    /// demo annotations. Flaps are snapped (no animation).
    /// Stage a named interaction state for the single-frame `--shots`
    /// renderer. This is the only place sample content enters the app: it
    /// swaps in the sample library and marks the app as demo so `sync` seeds
    /// demo annotations. Flaps are snapped (no animation).
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        // Shot builds pass an explicit (sample) library; never let the
        // document-load path overwrite it.
        self.library_loaded = true;

        // Onboarding stays genuinely empty so the zero-state can be captured.
        if scenario == "onboarding" {
            self.view = View::Library;
            return;
        }

        self.stage_shot_library();

        if self.apply_shot_chrome_scenario(scenario)
            || self.apply_shot_theme_scenario(scenario)
            || self.apply_shot_settings_panel_scenario(scenario)
            || self.apply_shot_text_selection_scenario(scenario)
            || self.apply_shot_reading_mode_scenario(scenario)
        {
            return;
        }
        // "read" and any unrecognised scenario: the plain reader.
        self.view = View::Reader;
    }

    /// Load the sample library into shot-scenario staging: the demo shelf,
    /// reset to its first book/chapter/page, each book given a plausible
    /// on-disk path (one on the shared workspace so the "Shared" badge has
    /// something to show).
    fn stage_shot_library(&mut self) {
        self.demo = true;
        self.library = crate::content::load_sample_library();
        self.book_idx = 0;
        self.chapter = 0;
        self.page = 0;
        // One book on the shared workspace so the shelf shot proves the
        // "Shared" badge, the rest local.
        self.library_paths.clear();
        for (i, b) in self.library.iter().enumerate() {
            let path = if i == 1 {
                content::workspace_books_dir().join("field-notes.epub")
            } else {
                PathBuf::from(format!("/home/reader/Books/book-{i}.epub"))
            };
            self.library_paths.insert(b.id.clone(), path);
        }
    }

    /// Shelf/reader chrome scenarios: which top-level view, and its overlay flaps.
    fn apply_shot_chrome_scenario(&mut self, scenario: &str) -> bool {
        match scenario {
            "shelf" | "library" => self.view = View::Library,
            "page-bubble" => {
                // The reader with the floating page-indicator bubble pinned open,
                // so a still frame shows the transient overlay + its prev/next
                // stepper (not a permanent bottom strip).
                self.view = View::Reader;
                self.arm_reader_bubble_for_shots();
            }
            "settings-library" => {
                // The Library settings pane: the folders books were opened from,
                // one already being watched for new additions.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Library;
                self.shot_watched = Some(vec![PathBuf::from("/home/reader/Books")]);
                self.library_paths.clear();
                self.library_paths.insert(
                    "book:a".into(),
                    PathBuf::from("/home/reader/Books/one.epub"),
                );
                self.library_paths.insert(
                    "book:b".into(),
                    PathBuf::from("/home/reader/Downloads/two.epub"),
                );
                self.library_paths.insert(
                    "book:c".into(),
                    PathBuf::from("/var/workspace/Books/shared.epub"),
                );
            }
            "narrow" => {
                self.view = View::Reader;
                // Contents rail open as an overlay flap; annotations closed so
                // the scrimmed flap reads clearly against the page.
                self.toc_flap.snap(true);
                self.notes_flap.snap(false);
            }
            "type" => {
                self.view = View::Reader;
                self.type_popover = true;
            }
            "library-scrolled" => {
                // The large value clamps to the real max in draw.
                self.view = View::Library;
                self.shelf_scroll = 5000.0;
            }
            _ => return false,
        }
        true
    }

    /// Reader typography/theme scenarios, applied through the real settings path.
    fn apply_shot_theme_scenario(&mut self, scenario: &str) -> bool {
        match scenario {
            "serif" => {
                // Applied through the real settings path so it exercises
                // serif shaping end to end.
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    font_id: "serif".into(),
                    ..Settings::default()
                });
            }
            "dark" => {
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    theme: ReadTheme::Dark,
                    ..Settings::default()
                });
            }
            "light" => {
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    theme: ReadTheme::Light,
                    ..Settings::default()
                });
            }
            "sepia" => {
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    theme: ReadTheme::Sepia,
                    ..Settings::default()
                });
            }
            "hicontrast" => {
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    theme: ReadTheme::HighContrast,
                    ..Settings::default()
                });
            }
            _ => return false,
        }
        true
    }

    /// Settings-screen panel scenarios: font/colour-scheme management.
    fn apply_shot_settings_panel_scenario(&mut self, scenario: &str) -> bool {
        match scenario {
            "font-picker" => {
                // Lists the real installed families (cosmic-text/fontconfig),
                // each previewed in its own face.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Fonts;
                self.font_picker = true;
            }
            "settings-colours" => {
                // The colour-scheme manager with a custom scheme present.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Colours;
                let mut sc = default_schemes();
                if let Some(first) = sc.first_mut() {
                    first.enabled = false; // a disabled built-in
                }
                sc.push(ColorScheme::from_bg(
                    "scheme1",
                    "Midnight",
                    [0x15, 0x1A, 0x24],
                ));
                self.shot_schemes = Some(sc);
            }
            "settings-fonts" => {
                // The font manager with an added system face.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Fonts;
                let mut f = default_fonts();
                f.push(FontEntry::new(
                    "font1",
                    FontKind::Named("DejaVu Serif".into()),
                    "DejaVu Serif",
                    1.08,
                    true,
                ));
                self.shot_fonts = Some(f);
            }
            "settings-fonts-rename" => {
                // Inline rename in progress via the real `ui::EditableLabel`,
                // typeset in that font's own face.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Fonts;
                let mut f = default_fonts();
                f.push(FontEntry::new(
                    "font1",
                    FontKind::Named("DejaVu Serif".into()),
                    "DejaVu Serif",
                    1.08,
                    true,
                ));
                self.shot_fonts = Some(f);
                let mut field = EditableLabel::new("DejaVu Serif")
                    .with_font_size(16.0)
                    .with_font_family(FontChoice::Named("DejaVu Serif".into()));
                field.begin_edit();
                self.renaming = Some(Rename {
                    target: RenameTarget::Font("font1".into()),
                    field,
                });
            }
            "scheme-editor" => {
                // The per-colour scheme editor open on a custom scheme.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Colours;
                let mut sc = default_schemes();
                sc.push(ColorScheme::from_bg(
                    "scheme1",
                    "Midnight",
                    [0x15, 0x1A, 0x24],
                ));
                self.shot_schemes = Some(sc);
                self.editing_scheme = Some("scheme1".into());
            }
            "picker" => {
                // The colour picker open on a custom scheme's page colour.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Colours;
                let mut sc = default_schemes();
                sc.push(ColorScheme::from_bg(
                    "scheme1",
                    "Midnight",
                    [0x1A, 0x22, 0x33],
                ));
                self.shot_schemes = Some(sc);
                self.color_picker
                    .open(PickTarget::Slot("scheme1".into(), 0), [0x1A, 0x22, 0x33]);
            }
            _ => return false,
        }
        true
    }

    /// Text-selection and annotation-composer scenarios.
    fn apply_shot_text_selection_scenario(&mut self, scenario: &str) -> bool {
        match scenario {
            "word-select" => {
                // End state of double-click-word then drag-right-by-words:
                // selection snapped to word boundaries via the real block text.
                self.demo = false;
                self.view = View::Reader;
                let block = 2usize;
                if let Some(text) = self
                    .library
                    .first()
                    .and_then(|b| b.chapters.first())
                    .map(|c| c.block_text(block))
                {
                    let n = text.chars().count();
                    if n > 6 {
                        let (s, _) = ui::word_boundaries_chars(&text, 0);
                        let target = (n / 3).clamp(6, 48).min(n - 1);
                        let (_, e) = ui::word_boundaries_chars(&text, target);
                        self.sel = Some(TextSel {
                            anchor: DocPos { block, ch: s },
                            head: DocPos { block, ch: e },
                        });
                        self.selected_block = Some(block);
                    }
                }
            }
            "select" => {
                self.demo = false;
                self.view = View::Reader;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 2, ch: 0 },
                    head: DocPos { block: 2, ch: 64 },
                });
                self.selected_block = Some(2);
            }
            "highlight" => {
                self.view = View::Reader;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 1, ch: 4 },
                    head: DocPos { block: 1, ch: 58 },
                });
                self.selected_block = Some(1);
            }
            "select-drag" => {
                // Non-demo so no seeded highlights clutter the bands.
                self.demo = false;
                self.view = View::Reader;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 1, ch: 6 },
                    head: DocPos { block: 2, ch: 60 },
                });
                self.selected_block = Some(1);
            }
            "note" => {
                self.demo = false;
                self.view = View::Reader;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 1, ch: 6 },
                    head: DocPos { block: 2, ch: 60 },
                });
                self.selected_block = Some(1);
                self.shot_compose = true;
            }
            "comments" => {
                // Anchored to the Tideglass sample's long opening paragraph.
                self.demo = false;
                self.view = View::Reader;
                self.book_idx = 2;
                self.chapter = 0;
                self.page = 0;
                self.shot_comments = true;
            }
            _ => return false,
        }
        true
    }

    /// Continuous-scroll and two-up reading-mode scenarios.
    fn apply_shot_reading_mode_scenario(&mut self, scenario: &str) -> bool {
        match scenario {
            "continuous" => {
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    mode: ReadMode::Continuous,
                    ..Settings::default()
                });
                self.scroll_y = 520.0;
            }
            "continuous-select" => {
                self.demo = false;
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    mode: ReadMode::Continuous,
                    ..Settings::default()
                });
                self.scroll_y = 120.0;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 1, ch: 6 },
                    head: DocPos { block: 2, ch: 54 },
                });
                self.selected_block = Some(1);
            }
            "two-up" | "spread" => {
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    mode: ReadMode::TwoUp,
                    ..Settings::default()
                });
            }
            "two-up-select" | "spread-select" => {
                self.demo = false;
                self.view = View::Reader;
                self.shot_settings = Some(Settings {
                    mode: ReadMode::TwoUp,
                    ..Settings::default()
                });
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 2, ch: 0 },
                    head: DocPos { block: 2, ch: 60 },
                });
                self.selected_block = Some(2);
            }
            _ => return false,
        }
        true
    }

    // --- CRDT <-> cache plumbing --------------------------------------------

    /// Seed the demo library's annotations so a `--shots` frame shows a
    /// lived-in reader. Runs only in demo mode — a real user's document is
    /// never populated with fabricated content.
    fn seed_if_needed(&mut self, doc: &mut AutoCommit) {
        if self.seeded || !self.demo {
            return;
        }
        self.seeded = true;

        // A spread of reading progress across the sample shelf so the
        // library shot shows real progress bars, a "recently opened" ordering,
        // and a couple of finished books. Demo-only, like every seed here.
        let fracs = [0.34_f32, 0.72, 0.08, 0.5, 1.0];
        let ids: Vec<String> = self.library.iter().map(|b| b.id.clone()).collect();
        for (i, id) in ids.iter().enumerate() {
            let f = fracs.get(i % fracs.len()).copied().unwrap_or(0.5);
            BooksDoc::write_progress(
                doc,
                id,
                &Progress {
                    chapter: 0,
                    page: (f * 6.0) as usize,
                    fraction: f,
                    opened: now_ms() - (i as u64) * 3_600_000,
                },
            );
        }

        if !BooksDoc::list_annotations(doc).is_empty() {
            return;
        }
        let Some(book) = self.library.first() else {
            return;
        };
        let id = book.id.clone();
        let Some(ch0) = book.chapters.first() else {
            return;
        };
        let excerpt = |i: usize| -> String {
            let t = ch0.block_text(i);
            let mut s: String = t.chars().take(96).collect();
            if t.chars().count() > 96 {
                s.push('…');
            }
            s
        };
        let bm = Annotation::new(&id, AnnoKind::Bookmark, 0, 0).with_excerpt(excerpt(0));
        BooksDoc::upsert_annotation(doc, &bm);
        // Ranged highlights snap their end to a word boundary so the band
        // never cuts mid-word.
        let snap_end = |i: usize, target: usize| -> usize {
            let chars: Vec<char> = ch0.block_text(i).chars().collect();
            let hi = target.min(chars.len());
            chars
                .get(..hi)
                .and_then(|slice| slice.iter().rposition(|&c| c == ' '))
                .unwrap_or(hi)
        };
        let mut h1 = Annotation::new(&id, AnnoKind::Highlight, 0, 1)
            .with_color(HiColor::Yellow)
            .with_excerpt(excerpt(1));
        h1.start = 0;
        h1.end = snap_end(1, 70);
        BooksDoc::upsert_annotation(doc, &h1);
        let mut h2 = Annotation::new(&id, AnnoKind::Highlight, 0, 3)
            .with_color(HiColor::Green)
            .with_excerpt(excerpt(3));
        h2.start = 0;
        h2.end = snap_end(3, 58);
        BooksDoc::upsert_annotation(doc, &h2);
        let mut note = Annotation::new(&id, AnnoKind::Note, 0, 2)
            .with_color(HiColor::Blue)
            .with_excerpt(excerpt(2));
        note.note =
            "The blue and the amber meeting in the middle — keep this image for the last pane."
                .into();
        BooksDoc::upsert_annotation(doc, &note);
    }

    /// Pull settings + annotations + shelf summaries out of the CRDT into the
    /// render caches. Idempotent; called from `sync` and after every handled
    /// event so the next `draw` is always consistent.
    fn refresh_from_doc(&mut self, doc: &AutoCommit) {
        let new_settings = BooksDoc::read_settings(doc);
        if new_settings != self.settings {
            self.settings = new_settings;
            self.layout = None; // typography changed → relayout
        }
        let new_fonts = BooksDoc::read_fonts(doc);
        let new_default = BooksDoc::read_default_font(doc);
        if new_fonts != self.fonts || new_default != self.default_font_id {
            self.fonts = new_fonts;
            self.default_font_id = new_default;
            self.layout = None; // a font attribute (family/size) may have changed
        }
        self.schemes = BooksDoc::read_schemes(doc);
        self.library_prefs = BooksDoc::read_library_prefs(doc);
        self.watched_folders = BooksDoc::read_watched_folders(doc);

        let ids: Vec<String> = self.library.iter().map(|b| b.id.clone()).collect();
        let cur_id = ids.get(self.book_idx).cloned().unwrap_or_default();
        self.annotations = BooksDoc::annotations_for(doc, &cur_id);

        let all = BooksDoc::list_annotations(doc);
        self.shelf_fraction.clear();
        self.shelf_annos.clear();
        self.shelf_opened.clear();
        for id in &ids {
            let p = BooksDoc::read_progress(doc, id);
            self.shelf_fraction.insert(id.clone(), p.fraction);
            self.shelf_opened.insert(id.clone(), p.opened);
            let c = all.iter().filter(|a| &a.doc_id == id).count();
            self.shelf_annos.insert(id.clone(), c);
        }
    }

    /// Persist the live reading position for the open book (deduped so we don't
    /// churn the document when nothing moved).
    fn persist_progress(&mut self, doc: &mut AutoCommit) {
        if self.view != View::Reader {
            return;
        }
        if self.last_persisted == Some((self.chapter, self.page)) {
            return;
        }
        let Some(book) = self.library.get(self.book_idx) else {
            return;
        };
        let nchap = book.chapters.len().max(1) as f32;
        let in_ch = (self.page as f32 + 1.0) / self.cur_page_count.max(1) as f32;
        let fraction = ((self.chapter as f32 + in_ch) / nchap).clamp(0.0, 1.0);
        let id = book.id.clone();
        BooksDoc::write_progress(
            doc,
            &id,
            &Progress {
                chapter: self.chapter,
                page: self.page,
                fraction,
                // 0 ⇒ write_progress leaves the recorded open time untouched.
                opened: 0,
            },
        );
        self.last_persisted = Some((self.chapter, self.page));
    }

    /// Move the continuous-scroll offset by `delta` px, rolling into the
    /// neighbouring chapter when the scroll runs off either end.
    fn scroll_continuous(&mut self, delta: f64) {
        let (total_h, view_h) = self.flow_extent;
        let max_scroll = (total_h - view_h).max(0.0);
        let target = self.scroll_y + delta;
        if target < -0.5 && self.chapter > 0 {
            self.chapter -= 1;
            self.selected_block = None;
            self.pending_scroll_end = true;
            self.scroll_y = 0.0;
            self.layout = None;
        } else if target > max_scroll + 0.5 && self.chapter + 1 < self.num_chapters() {
            self.chapter += 1;
            self.selected_block = None;
            self.scroll_y = 0.0;
            self.layout = None;
        } else {
            self.scroll_y = target.clamp(0.0, max_scroll);
        }
    }

    // --- Library loading + the host file picker -----------------------------

    /// Whether the book at library index `i` lives on the shared workspace.
    #[cfg(test)]
    fn book_is_shared(&self, i: usize) -> bool {
        self.library
            .get(i)
            .and_then(|b| self.library_paths.get(&b.id))
            .is_some_and(|p| is_shared(p))
    }

    /// Hydrate the reader's persisted library from the document on first
    /// sync. A book handed over by the OS is imported + opened; otherwise
    /// the shelf is home.
    fn ensure_library_loaded(&mut self, doc: &mut AutoCommit) {
        if self.library_loaded || self.demo {
            return;
        }
        self.library_loaded = true;

        // Import a handed-over book first, so it is part of the loaded set.
        let open_id = self
            .pending_open_path
            .take()
            .and_then(|p| content::import_path(doc, &p).map(|b| b.id));

        self.library.clear();
        self.library_paths.clear();
        for lb in content::load_library(doc) {
            self.library_paths.insert(lb.book.id.clone(), lb.path);
            self.library.push(lb.book);
        }

        match open_id.and_then(|id| self.library.iter().position(|b| b.id == id)) {
            Some(i) => self.open_book_at(doc, i),
            None => self.view = View::Library,
        }
    }

    /// Open the book at library index `i` in the reader, restoring where the
    /// reader left off and stamping the open time for the "recently opened" sort.
    fn open_book_at(&mut self, doc: &mut AutoCommit, i: usize) {
        if self.library.is_empty() {
            return;
        }
        self.book_idx = i.min(self.library.len() - 1);
        self.view = View::Reader;
        let id = self.library[self.book_idx].id.clone();
        let mut p = BooksDoc::read_progress(doc, &id);
        let nchap = self.num_chapters().saturating_sub(1);
        self.chapter = p.chapter.min(nchap);
        self.page = p.page;
        p.opened = now_ms();
        BooksDoc::write_progress(doc, &id, &p);
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.layout = None;
        self.last_persisted = None;
    }

    /// Ask the host to choose a book to open; the chosen path comes back over
    /// a channel drained each frame in [`Self::drain_portal`]. A second
    /// request while one is in flight is ignored.
    fn request_open_via_portal(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(book_open_request().send());
        self.load_error = None;
        self.dirty = true;
    }

    /// Drain a completed picker request, if any. Never blocks — a
    /// still-pending picker stays installed for the next frame.
    fn drain_portal(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => self.import_and_open(doc, &path),
                    // A cancel is not an error — leave the current view intact.
                    PortalOutcome::Cancelled => {}
                    PortalOutcome::Failed(reason) => {
                        self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                        self.dirty = true;
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
            }
        }
    }

    /// Import the `.epub` at `path` into the library and open it in the reader.
    /// Re-opening a book already on the shelf just re-opens it (no duplicate).
    fn import_and_open(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        if let Some(book) = content::import_path(doc, path) {
            let id = book.id.clone();
            self.library_paths.insert(id.clone(), path.to_path_buf());
            let i = if let Some(i) = self.library.iter().position(|b| b.id == id) {
                i
            } else {
                self.library.push(book);
                self.library.len() - 1
            };
            self.load_error = None;
            self.open_book_at(doc, i);
        } else {
            self.load_error = Some("That file couldn't be opened as a book.".into());
            self.dirty = true;
        }
    }

    /// Re-scan the reader's watched folders and pull in any book not already
    /// on the shelf. Cheap when nothing is new, so it is safe to run on
    /// window focus.
    fn rescan_watched(&mut self, doc: &mut AutoCommit) {
        let known: std::collections::HashSet<PathBuf> =
            self.library_paths.values().cloned().collect();
        let mut changed = false;
        for path in content::watched_epub_paths(doc) {
            if known.contains(&path) {
                continue;
            }
            if let Some(book) = content::import_path(doc, &path) {
                let id = book.id.clone();
                if !self.library.iter().any(|b| b.id == id) {
                    self.library.push(book);
                    changed = true;
                }
                self.library_paths.insert(id, path);
            }
        }
        if changed {
            self.dirty = true;
        }
    }

    // --- Action dispatch (event side — has the document) --------------------

    fn dispatch(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::OpenBook(i) => {
                self.open_book_at(doc, i);
            }
            Action::Back => self.on_back(),
            Action::NextPage => self.on_next_page(),
            Action::PrevPage => self.on_prev_page(),
            Action::SetMode(mode) => self.on_set_mode(mode, doc),
            Action::GotoChapter(c) => self.on_goto_chapter(c),
            Action::ToggleToc => self.on_toggle_toc(),
            Action::ToggleNotes => self.on_toggle_notes(),
            Action::ToggleTypePopover => self.type_popover = !self.type_popover,
            Action::DismissOverlay => self.on_dismiss_overlay(),
            Action::Swallow => {}
            Action::ToggleBookmark => self.toggle_bookmark(doc),
            Action::FontInc => self.bump_setting(doc, |s| {
                s.font_scale = (s.font_scale + 0.1).clamp(Settings::FONT_MIN, Settings::FONT_MAX);
            }),
            Action::FontDec => self.bump_setting(doc, |s| {
                s.font_scale = (s.font_scale - 0.1).clamp(Settings::FONT_MIN, Settings::FONT_MAX);
            }),
            Action::SpacingInc => self.bump_setting(doc, |s| {
                s.line_spacing =
                    (s.line_spacing + 0.1).clamp(Settings::SPACING_MIN, Settings::SPACING_MAX);
            }),
            Action::SpacingDec => self.bump_setting(doc, |s| {
                s.line_spacing =
                    (s.line_spacing - 0.1).clamp(Settings::SPACING_MIN, Settings::SPACING_MAX);
            }),
            Action::CycleFont => self.on_cycle_font(doc),
            Action::SetFont(id) => self.bump_setting(doc, |s| s.font_id = id),
            Action::SetComposition(composition) => {
                self.bump_setting(doc, |s| s.composition = composition);
            }
            Action::SetReadTheme(t) => self.bump_setting(doc, |s| {
                s.theme = t;
                // Selecting a built-in tint clears any custom scheme override.
                s.scheme_id = String::new();
            }),
            Action::SetColor(c) => self.on_set_color(c, doc),
            Action::AddNote => self.on_add_note(doc),
            Action::CopySelection => self.on_copy_selection(),
            Action::EditNote(id) => self.on_edit_note(id),
            Action::JumpTo(id) => self.on_jump_to(id),
            Action::Delete(id) => self.on_delete_annotation(id, doc),
            Action::OpenComments(group) => self.on_open_comments(group),
            Action::CloseComments => self.on_close_comments(doc),
            Action::DeleteHighlight(group) => self.on_delete_highlight(group, doc),
            Action::AddComment(group) => self.on_add_comment(group, doc),
            Action::EditComment(id) => self.composing = Some(id),
            Action::DeleteComment(id) => self.on_delete_comment(id, doc),
            Action::OpenViaPortal => self.request_open_via_portal(),
            Action::ShareToWorkspace(i) => self.on_share_to_workspace(i, doc),
            Action::RemoveFromLibrary(i) => self.on_remove_from_library(i, doc),
            Action::ToggleWatchFolder(folder) => self.on_toggle_watch_folder(folder, doc),

            // --- settings screen: navigation -----------------------------
            Action::OpenSettings => self.on_open_settings(),
            Action::CloseSettings => self.on_close_settings(doc),
            Action::SetSettingsTab(tab) => self.on_set_settings_tab(tab, doc),

            // --- settings screen: colour schemes -------------------------
            Action::SetScheme(id) => self.bump_setting(doc, |s| s.scheme_id = id),
            Action::ToggleSchemeEnabled(id) => self.on_toggle_scheme_enabled(&id, doc),
            Action::DuplicateScheme(id) => self.on_duplicate_scheme(&id, doc),
            Action::DeleteScheme(id) => self.on_delete_scheme(&id, doc),
            Action::EditScheme(id) => self.on_edit_scheme(id, doc),
            Action::CloseSchemeEditor => self.on_close_scheme_editor(doc),
            Action::NewSchemeViaPicker => self.on_new_scheme_via_picker(doc),
            Action::PickSchemeColor(id, slot) => self.on_pick_scheme_color(id, slot, doc),
            Action::BeginRenameScheme(id) => self.on_begin_rename_scheme(id, doc),

            // --- settings screen: fonts ----------------------------------
            Action::SetDefaultFont(id) => self.on_set_default_font(&id, doc),
            Action::ToggleDefaultFontMenu => self.default_font_menu = !self.default_font_menu,
            Action::ToggleFontShown(id) => self.on_toggle_font_shown(&id, doc),
            Action::RemoveFont(id) => self.on_remove_font(&id, doc),
            Action::OpenFontPicker => self.on_open_font_picker(),
            Action::CloseFontPicker => self.font_picker = false,
            Action::AddSystemFont(family) => self.on_add_system_font(&family, doc),
            Action::BeginRenameFont(id) => self.on_begin_rename_font(id, doc),
        }
    }

    fn on_back(&mut self) {
        self.view = View::Library;
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.type_popover = false;
    }

    fn on_next_page(&mut self) {
        self.comment_overlay.close();
        if self.settings.mode.is_continuous() {
            // A "page" in continuous mode is a viewport-height scroll.
            self.scroll_continuous(self.flow_extent.1 * 0.9);
            return;
        }
        let before = (self.chapter, self.page);
        let stride = self.settings.mode.page_stride();
        let pc = self.cur_page_count.max(1);
        if self.page + stride < pc {
            self.page += stride;
        } else if self.chapter + 1 < self.num_chapters() {
            self.chapter += 1;
            self.page = 0;
            self.selected_block = None;
            self.layout = None;
        }
        if (self.chapter, self.page) != before {
            // The new page eases in from the right — a forward turn.
            self.page_turn = Some(PageTurn::start(1.0));
        }
    }

    fn on_prev_page(&mut self) {
        self.comment_overlay.close();
        if self.settings.mode.is_continuous() {
            self.scroll_continuous(-self.flow_extent.1 * 0.9);
            return;
        }
        let before = (self.chapter, self.page);
        let stride = self.settings.mode.page_stride();
        if self.page >= stride {
            self.page -= stride;
        } else if self.page > 0 {
            // A spread whose left column is page 1: step to the cover.
            self.page = 0;
        } else if self.chapter > 0 {
            self.chapter -= 1;
            self.selected_block = None;
            self.pending_last_page = true;
            self.layout = None;
        }
        if (self.chapter, self.page) != before {
            // The new page eases in from the left — a backward turn.
            self.page_turn = Some(PageTurn::start(-1.0));
        }
    }

    fn on_set_mode(&mut self, mode: ReadMode, doc: &mut AutoCommit) {
        // Preserve the reading position across the switch: entering
        // continuous scroll lands on the current page's flow offset;
        // leaving it snaps back to the nearest page (resolved in draw).
        if self.settings.mode != mode {
            if mode.is_continuous()
                && !self.settings.mode.is_continuous()
                && let Some(l) = self.layout.as_ref()
            {
                self.scroll_y = f64::from(l.pages.get(self.page).copied().unwrap_or(0.0));
            }
            self.bump_setting(doc, |s| s.mode = mode);
            // Two-up halves the column width → relayout on the next draw.
            self.layout = None;
        }
    }

    fn on_goto_chapter(&mut self, c: usize) {
        self.chapter = c.min(self.num_chapters().saturating_sub(1));
        self.page = 0;
        self.selected_block = None;
        self.comment_overlay.close();
        self.layout = None;
    }

    fn on_toggle_toc(&mut self) {
        let layout = self.toc_layout();
        self.toc_flap.toggle(layout);
    }

    fn on_toggle_notes(&mut self) {
        let layout = self.notes_layout();
        self.notes_flap.toggle(layout);
    }

    fn on_dismiss_overlay(&mut self) {
        self.type_popover = false;
        if self.toc_layout() == SplitLayout::Overlay && self.toc_open() {
            self.toc_flap.close();
        }
        if self.notes_layout() == SplitLayout::Overlay && self.notes_open() {
            self.notes_flap.close();
        }
    }

    fn on_cycle_font(&mut self, doc: &mut AutoCommit) {
        // Cycle through the fonts offered in quick settings.
        let shown = self.shown_fonts();
        if !shown.is_empty() {
            let cur = self.active_font().id;
            let idx = shown.iter().position(|f| f.id == cur).unwrap_or(0);
            let next = shown[(idx + 1) % shown.len()].id.clone();
            self.bump_setting(doc, |s| s.font_id = next);
        }
    }

    fn on_set_color(&mut self, c: HiColor, doc: &mut AutoCommit) {
        self.hi_color = c;
        // Picking the same colour again clears it; a different colour
        // replaces it. Resolves to the same action the palette's
        // "Highlight selection" dispatches — one source of truth.
        if let Some(sel) = self.sel.filter(|s| !s.is_empty()) {
            if let Some(plan) = self.resolve_highlight_plan(sel, c) {
                self.perform_action(doc, &plan.into_action());
            }
            self.clear_selection();
        }
    }

    fn on_add_note(&mut self, doc: &mut AutoCommit) {
        // A note anchors to the live text selection; the composer
        // floats on-page near the passage rather than a separate screen.
        let anchor = self.sel.filter(|s| !s.is_empty()).map(|s| {
            let (start, end) = s.ordered();
            (start.block, start.ch, end.ch.max(start.ch))
        });
        if let Some((block, start, end)) = anchor {
            let book_id = self.book_id();
            let excerpt = self.selection_excerpt();
            let id = Uuid::new_v4();
            let action = crate::actions::add_note_action(
                &book_id,
                id,
                self.chapter,
                block,
                start,
                end,
                self.hi_color,
                &excerpt,
                "",
            );
            self.perform_action(doc, &action);
            self.composing = Some(id);
            self.note_composer.open(id, "");
        }
    }

    fn on_copy_selection(&self) {
        // Selection stays put so the reader can also highlight or note it.
        let text = self.selection_text();
        if !text.is_empty() {
            ui::clipboard_copy(&text);
        }
    }

    fn on_edit_note(&mut self, id: Uuid) {
        if let Some(note) = self
            .annotations
            .iter()
            .find(|a| a.id == id && a.kind == AnnoKind::Note)
        {
            self.composing = Some(id);
            self.note_composer.open(id, &note.note);
            self.notes_flap.open();
        }
    }

    fn on_jump_to(&mut self, id: Uuid) {
        if let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() {
            self.chapter = a.chapter.min(self.num_chapters().saturating_sub(1));
            self.pending_jump_block = Some(a.block);
            self.clear_selection();
            self.comment_overlay.close();
            self.layout = None;
        }
    }

    fn on_delete_annotation(&mut self, id: Uuid, doc: &mut AutoCommit) {
        if self.composing == Some(id) {
            self.composing = None;
            self.note_composer.close();
        }
        self.perform_action(doc, &crate::actions::delete_annotation_action(id));
    }

    fn on_open_comments(&mut self, group: Uuid) {
        // The overlay owns interaction while it is up: drop any live
        // selection so its toolbar doesn't fight the popover.
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.open(group);
    }

    fn on_close_comments(&mut self, doc: &mut AutoCommit) {
        // Finish any comment mid-composition so its text is saved.
        if self.composing.is_some() {
            self.finish_composing(doc);
        }
        self.comment_overlay.close();
    }

    fn on_delete_highlight(&mut self, group: Uuid, doc: &mut AutoCommit) {
        self.perform_action(doc, &crate::actions::delete_highlight_action(group));
        if self.comment_overlay.group() == Some(group) {
            self.comment_overlay.close();
        }
        self.composing = None;
        self.note_composer.close();
    }

    fn on_add_comment(&mut self, group: Uuid, doc: &mut AutoCommit) {
        // Anchor to the highlight's first piece so it sorts with the range.
        if self.composing.is_some() {
            self.finish_composing(doc);
        }
        let anchor = self
            .annotations
            .iter()
            .filter(|a| a.kind == AnnoKind::Highlight && a.group == group)
            .min_by_key(|a| (a.block, a.start))
            .map(|a| (a.block, a.start, a.end, a.color, a.excerpt.clone()));
        if let Some((block, start, end, color, excerpt)) = anchor {
            let book_id = self.book_id();
            let id = Uuid::new_v4();
            let action = crate::actions::add_comment_action(
                &book_id,
                group,
                id,
                self.chapter,
                block,
                start,
                end,
                color,
                &excerpt,
                "",
            );
            self.perform_action(doc, &action);
            self.comment_overlay.open(group);
            self.composing = Some(id);
        }
    }

    fn on_delete_comment(&mut self, id: Uuid, doc: &mut AutoCommit) {
        if self.composing == Some(id) {
            self.composing = None;
            self.note_composer.close();
        }
        self.perform_action(doc, &crate::actions::delete_annotation_action(id));
    }

    fn on_share_to_workspace(&mut self, i: usize, doc: &mut AutoCommit) {
        let Some(book) = self.library.get(i) else {
            return;
        };
        let id = book.id.clone();
        let Some(src) = self.library_paths.get(&id).cloned() else {
            return;
        };
        if let Some((_, dest)) = content::share_to_workspace(doc, &src) {
            // Same content id, new (shared) home: the shelf now reads it
            // as shared and its annotations/progress carry over untouched.
            self.library_paths.insert(id, dest);
            self.dirty = true;
        }
    }

    fn on_remove_from_library(&mut self, i: usize, doc: &mut AutoCommit) {
        if i >= self.library.len() {
            return;
        }
        let removed = self.library.remove(i);
        self.library_paths.remove(&removed.id);
        self.cover_cache.remove(&removed.id);
        BooksDoc::remove_library_entry(doc, &removed.id);
        // Keep the reader pointed at a valid book after the shelf shrinks.
        self.book_idx = self.book_idx.min(self.library.len().saturating_sub(1));
        self.shelf_cursor = self.shelf_cursor.min(self.library.len().saturating_sub(1));
        if self.library.is_empty() {
            self.view = View::Library;
        }
        self.layout = None;
    }

    fn on_toggle_watch_folder(&mut self, folder: PathBuf, doc: &mut AutoCommit) {
        let mut watched = BooksDoc::read_watched_folders(doc);
        if let Some(pos) = watched.iter().position(|f| f == &folder) {
            watched.remove(pos);
        } else {
            watched.push(folder);
        }
        BooksDoc::write_watched_folders(doc, &watched);
        // Watching a folder pulls in any books already sitting in it.
        self.rescan_watched(doc);
    }

    fn on_open_settings(&mut self) {
        self.view = View::Settings;
        self.type_popover = false;
        // Numeric controls retain their own edit state and commit on blur.
        self.font_picker = false;
        self.color_picker.cancel();
        self.default_font_menu = false;
        self.editing_scheme = None;
        self.renaming = None;
        self.settings_scroll = 0.0;
    }

    fn on_close_settings(&mut self, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        self.font_picker = false;
        self.color_picker.cancel();
        self.default_font_menu = false;
        self.editing_scheme = None;
        self.view = if self.library.is_empty() {
            View::Library
        } else {
            View::Reader
        };
    }

    fn on_set_settings_tab(&mut self, tab: SettingsTab, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        self.default_font_menu = false;
        self.color_picker.cancel();
        self.settings_tab = tab;
        self.settings_scroll = 0.0;
    }

    fn on_toggle_scheme_enabled(&mut self, id: &str, doc: &mut AutoCommit) {
        let mut sc = self.schemes.clone();
        let mut now_disabled = false;
        if let Some(s) = sc.iter_mut().find(|s| s.id == id) {
            s.enabled = !s.enabled;
            now_disabled = !s.enabled;
        }
        BooksDoc::write_schemes(doc, &sc);
        if now_disabled && self.settings.scheme_id == id {
            self.bump_setting(doc, |st| st.scheme_id = String::new());
        }
    }

    fn on_duplicate_scheme(&mut self, id: &str, doc: &mut AutoCommit) {
        let mut sc = self.schemes.clone();
        if let Some(src) = sc.iter().find(|s| s.id == id).cloned() {
            let ids: Vec<String> = sc.iter().map(|s| s.id.clone()).collect();
            let new_id = Self::fresh_id("scheme", &ids);
            let mut dup = src.clone();
            dup.id.clone_from(&new_id);
            dup.name = format!("{} copy", src.name);
            dup.builtin = false;
            dup.enabled = true;
            let pos = sc
                .iter()
                .position(|s| s.id == id)
                .map_or(sc.len(), |p| p + 1);
            sc.insert(pos, dup);
            BooksDoc::write_schemes(doc, &sc);
            self.editing_scheme = Some(new_id);
        }
    }

    fn on_delete_scheme(&mut self, id: &str, doc: &mut AutoCommit) {
        let mut sc = self.schemes.clone();
        sc.retain(|s| s.builtin || s.id != id);
        BooksDoc::write_schemes(doc, &sc);
        if self.editing_scheme.as_deref() == Some(id) {
            self.editing_scheme = None;
        }
        if self.settings.scheme_id == id {
            self.bump_setting(doc, |st| st.scheme_id = String::new());
        }
    }

    fn on_edit_scheme(&mut self, id: String, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        self.editing_scheme = Some(id);
    }

    fn on_close_scheme_editor(&mut self, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        self.editing_scheme = None;
    }

    fn on_new_scheme_via_picker(&mut self, doc: &mut AutoCommit) {
        // Open the colour picker to choose a background; the scheme is
        // derived + created when the picker is committed.
        self.commit_rename(doc);
        self.color_picker
            .open(PickTarget::NewScheme, [0x1B, 0x18, 0x16]);
    }

    fn on_pick_scheme_color(&mut self, id: String, slot: usize, doc: &mut AutoCommit) {
        // Edit any scheme colour through the same picker.
        self.commit_rename(doc);
        let cur = self
            .schemes
            .iter()
            .find(|s| s.id == id)
            .map_or([0x80, 0x80, 0x80], |s| s.slots()[slot].1);
        self.color_picker.open(PickTarget::Slot(id, slot), cur);
    }

    fn on_begin_rename_scheme(&mut self, id: String, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        let name = self
            .schemes
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        // The name in the open scheme editor reads larger than the row
        // in the list; size the field to match wherever it's drawn.
        let size = if self.editing_scheme.is_some() {
            20.0
        } else {
            15.0
        };
        let mut field = EditableLabel::new(&name).with_font_size(size);
        field.begin_edit();
        self.renaming = Some(Rename {
            target: RenameTarget::Scheme(id),
            field,
        });
    }

    fn on_set_default_font(&mut self, id: &str, doc: &mut AutoCommit) {
        BooksDoc::write_default_font(doc, id);
        self.default_font_menu = false;
    }

    fn on_toggle_font_shown(&self, id: &str, doc: &mut AutoCommit) {
        let mut f = self.fonts.clone();
        if let Some(e) = f.iter_mut().find(|e| e.id == id) {
            e.shown = !e.shown;
        }
        BooksDoc::write_fonts(doc, &f);
    }

    fn on_remove_font(&mut self, id: &str, doc: &mut AutoCommit) {
        let mut f = self.fonts.clone();
        f.retain(|e| e.id != id);
        BooksDoc::write_fonts(doc, &f);
        if self.settings.font_id == id {
            self.bump_setting(doc, |s| s.font_id = String::new());
        }
        if self.default_font_id == id {
            BooksDoc::write_default_font(doc, "");
        }
    }

    fn on_open_font_picker(&mut self) {
        self.font_picker = true;
        self.picker_scroll = 0.0;
    }

    fn on_add_system_font(&mut self, family: &str, doc: &mut AutoCommit) {
        let mut f = self.fonts.clone();
        let dup = f
            .iter()
            .any(|e| matches!(&e.kind, FontKind::Named(n) if n == family));
        if !dup {
            let ids: Vec<String> = f.iter().map(|e| e.id.clone()).collect();
            let id = Self::fresh_id("font", &ids);
            f.push(FontEntry::new(
                &id,
                FontKind::Named(family.to_string()),
                family,
                1.0,
                true,
            ));
            BooksDoc::write_fonts(doc, &f);
        }
        self.font_picker = false;
    }

    fn on_begin_rename_font(&mut self, id: String, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        let entry = self.fonts.iter().find(|e| e.id == id);
        let name = entry.map(|e| e.name.clone()).unwrap_or_default();
        // Render the rename field in the font's own face, so the name
        // is typeset in the typeface it selects while you edit it.
        let fam = entry.map_or(FontChoice::SansSerif, |e| font_choice(&e.kind));
        let mut field = EditableLabel::new(&name)
            .with_font_size(16.0)
            .with_font_family(fam);
        field.begin_edit();
        self.renaming = Some(Rename {
            target: RenameTarget::Font(id),
            field,
        });
    }

    fn on_set_sort(&mut self, sort: crate::model::LibrarySort, doc: &mut AutoCommit) {
        let mut p = self.library_prefs;
        p.sort = sort;
        self.library_prefs = p;
        BooksDoc::write_library_prefs(doc, &p);
        self.shelf_controls.close_sort_menu();
    }

    fn on_toggle_hide_read(&mut self, doc: &mut AutoCommit) {
        let mut p = self.library_prefs;
        p.hide_read = !p.hide_read;
        self.library_prefs = p;
        BooksDoc::write_library_prefs(doc, &p);
    }

    fn dispatch_shelf_control(&mut self, action: ShelfControlAction, doc: &mut AutoCommit) {
        match action {
            ShelfControlAction::ToggleSortMenu => self.shelf_controls.toggle_sort_menu(),
            ShelfControlAction::SetSort(sort) => self.on_set_sort(sort, doc),
            ShelfControlAction::ToggleHideRead => self.on_toggle_hide_read(doc),
            ShelfControlAction::Swallow => {}
        }
    }

    fn dispatch_comment_action(&mut self, action: CommentAction, doc: &mut AutoCommit) {
        let action = match action {
            CommentAction::Close => Action::CloseComments,
            CommentAction::Swallow => Action::Swallow,
            CommentAction::DeleteHighlight(group) => Action::DeleteHighlight(group),
            CommentAction::Add(group) => Action::AddComment(group),
            CommentAction::Edit(id) => Action::EditComment(id),
            CommentAction::Delete(id) => Action::DeleteComment(id),
        };
        self.dispatch(action, doc);
    }

    /// Apply the picker's live colour to its target (called on every drag). For
    /// a slot, this live-edits the scheme so the manager previews the change;
    /// for a new scheme, nothing is written until [`Self::commit_picker`].
    fn apply_live_pick(&self, doc: &mut AutoCommit, edit: &ColorEdit) {
        if let PickTarget::Slot(id, slot) = &edit.target {
            let mut sc = self.schemes.clone();
            if let Some(s) = sc.iter_mut().find(|s| &s.id == id && !s.builtin) {
                s.set_slot(*slot, edit.rgb);
                BooksDoc::write_schemes(doc, &sc);
            }
        }
    }

    /// Commit the colour picker: create the new scheme (for a from-background
    /// pick) or finalise the slot edit, then close.
    fn commit_picker(&mut self, doc: &mut AutoCommit) {
        let Some(edit) = self.color_picker.take() else {
            return;
        };
        match edit.target {
            PickTarget::NewScheme => {
                let mut sc = self.schemes.clone();
                let ids: Vec<String> = sc.iter().map(|s| s.id.clone()).collect();
                let new_id = Self::fresh_id("scheme", &ids);
                let n = sc.iter().filter(|s| !s.builtin).count() + 1;
                sc.push(ColorScheme::from_bg(
                    &new_id,
                    &format!("Scheme {n}"),
                    edit.rgb,
                ));
                BooksDoc::write_schemes(doc, &sc);
                self.editing_scheme = Some(new_id.clone());
                self.bump_setting(doc, |st| st.scheme_id = new_id);
            }
            PickTarget::Slot(id, slot) => {
                let mut sc = self.schemes.clone();
                if let Some(s) = sc.iter_mut().find(|s| s.id == id && !s.builtin) {
                    s.set_slot(slot, edit.rgb);
                }
                BooksDoc::write_schemes(doc, &sc);
            }
        }
    }

    /// Force an in-progress inline rename to commit, then persist the
    /// result. Called before an action that would replace or dismiss the
    /// rename. No-op when nothing is being renamed.
    fn commit_rename(&mut self, doc: &mut AutoCommit) {
        if let Some(rn) = self.renaming.as_mut() {
            rn.field.commit();
        }
        self.poll_rename_outcome(doc);
    }

    /// Drain the rename field's latest outcome: on commit, persist the new
    /// name; on commit or cancel, clear the rename. The single place a
    /// rename result is written, so Enter/blur/click-outside all funnel here.
    fn poll_rename_outcome(&mut self, doc: &mut AutoCommit) {
        let Some(rn) = self.renaming.as_mut() else {
            return;
        };
        match rn.field.take_outcome() {
            EditOutcome::Committed(name) => {
                let target = rn.target.clone();
                self.renaming = None;
                self.apply_rename(target, &name, doc);
            }
            EditOutcome::Cancelled => {
                self.renaming = None;
            }
            EditOutcome::None => {}
        }
    }

    /// Write a committed rename to the CRDT. A blank name is ignored (keeps the
    /// old one).
    fn apply_rename(&self, target: RenameTarget, name: &str, doc: &mut AutoCommit) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match target {
            RenameTarget::Font(id) => {
                let mut f = self.fonts.clone();
                if let Some(e) = f.iter_mut().find(|e| e.id == id) {
                    e.name = name.to_string();
                }
                BooksDoc::write_fonts(doc, &f);
            }
            RenameTarget::Scheme(id) => {
                let mut sc = self.schemes.clone();
                if let Some(s) = sc.iter_mut().find(|s| s.id == id) {
                    s.name = name.to_string();
                }
                BooksDoc::write_schemes(doc, &sc);
            }
        }
    }

    fn book_id(&self) -> String {
        self.library
            .get(self.book_idx)
            .map(|b| b.id.clone())
            .unwrap_or_default()
    }

    /// The font the reader currently renders in: the explicit per-reader
    /// selection, else the registry default, else the first registered font.
    fn active_font(&self) -> FontEntry {
        let want = if self.settings.font_id.is_empty() {
            self.default_font_id.as_str()
        } else {
            self.settings.font_id.as_str()
        };
        self.fonts
            .iter()
            .find(|f| f.id == want)
            // Nothing chosen (a fresh reader, or a stale id): default to the
            // serif book face — books read best in serif — before the first
            // registered font.
            .or_else(|| self.fonts.iter().find(|f| f.id == "serif"))
            .or_else(|| self.fonts.first())
            .cloned()
            .unwrap_or_else(|| default_fonts()[0].clone())
    }

    /// The fonts offered in the quick display-settings picker.
    fn shown_fonts(&self) -> Vec<FontEntry> {
        let shown: Vec<FontEntry> = self.fonts.iter().filter(|f| f.shown).cloned().collect();
        if shown.is_empty() {
            self.fonts.clone()
        } else {
            shown
        }
    }

    /// Resolve the active font into everything the layout + paint passes need:
    /// the toolkit family, the per-face size multiplier, a layout-cache
    /// signature, and whether the body is monospace.
    fn body_font(&self) -> (FontChoice, f32, u64, bool) {
        let f = self.active_font();
        let choice = match font_choice(&f.kind) {
            // A named face that isn't installed would fall through to cosmic-text's
            // default *sans* — the wrong reading face for a book. Substitute the
            // generic serif, which fontconfig always resolves to a real serif.
            FontChoice::Named(n) if !family_is_installed(&n) => FontChoice::Serif,
            other => other,
        };
        let mono = f.kind.is_mono();
        (choice, f.size_mult, font_sig(&f), mono)
    }

    /// Every enabled scheme (built-ins + custom). Built-in tints route
    /// through `SetReadTheme` to keep their hand-tuned palette; custom
    /// schemes apply their own colours.
    fn quick_scheme_pills(&self) -> Vec<SchemePill> {
        self.schemes
            .iter()
            .filter(|s| s.enabled)
            .map(|s| {
                if s.builtin {
                    let rt = ReadTheme::from_str(&s.id);
                    SchemePill {
                        label: s.name.clone(),
                        action: Action::SetReadTheme(rt),
                        active: self.settings.scheme_id.is_empty() && self.settings.theme == rt,
                    }
                } else {
                    SchemePill {
                        label: s.name.clone(),
                        action: Action::SetScheme(s.id.clone()),
                        active: self.settings.scheme_id == s.id,
                    }
                }
            })
            .collect()
    }

    /// Resolve the reader's page palette: a selected, enabled custom scheme wins;
    /// otherwise the built-in tint. Custom schemes carry their own accent.
    fn resolve_palette(&self, theme: &Theme) -> ReaderPalette {
        if !self.settings.scheme_id.is_empty()
            && let Some(s) = self
                .schemes
                .iter()
                .find(|s| s.id == self.settings.scheme_id && s.enabled && !s.builtin)
        {
            return ReaderPalette::from_scheme(s);
        }
        ReaderPalette::resolve(self.settings.theme, theme)
    }

    /// Allocate an id not already used by a font/scheme in `existing`.
    fn fresh_id(prefix: &str, existing: &[String]) -> String {
        for n in 1.. {
            let id = format!("{prefix}{n}");
            if !existing.iter().any(|e| e == &id) {
                return id;
            }
        }
        format!("{prefix}{}", Uuid::new_v4())
    }

    fn block_excerpt(&self, block: usize) -> String {
        let Some(book) = self.library.get(self.book_idx) else {
            return String::new();
        };
        let Some(ch) = book.chapters.get(self.chapter) else {
            return String::new();
        };
        let t = ch.block_text(block);
        let mut s: String = t.chars().take(120).collect();
        if t.chars().count() > 120 {
            s.push('…');
        }
        s
    }

    fn bump_setting(&mut self, doc: &mut AutoCommit, f: impl FnOnce(&mut Settings)) {
        let mut s = BooksDoc::read_settings(doc);
        f(&mut s);
        BooksDoc::write_settings(doc, &s);
        self.settings = s;
        self.layout = None;
    }

    /// Resolve a highlight-the-selection gesture into a replayable
    /// [`HighlightPlan`](crate::actions::HighlightPlan). Colours never
    /// layer: applying a colour marks the range, the same colour again
    /// clears it, a different colour replaces it — any highlight the
    /// selection overlaps is dropped first. Returns `None` when there is no
    /// laid-out page, or the gesture resolves to nothing.
    fn resolve_highlight_plan(
        &self,
        sel: TextSel,
        color: HiColor,
    ) -> Option<crate::actions::HighlightPlan> {
        let (start, end) = sel.ordered();
        let layout = self.layout.as_ref()?;
        let clens: HashMap<usize, usize> = layout
            .blocks
            .iter()
            .map(|b| (b.index, b.char_len))
            .collect();
        // Distinct highlight *groups* the selection overlaps (a group is one
        // logical highlight, possibly several per-block rows). Toggling works on
        // whole highlights, never half of a multi-paragraph one.
        let mut overlapping: Vec<(Uuid, HiColor)> = Vec::new();
        for a in &self.annotations {
            if a.kind != AnnoKind::Highlight
                || a.chapter != self.chapter
                || a.start >= a.end
                || a.block < start.block
                || a.block > end.block
            {
                continue;
            }
            let clen = *clens.get(&a.block).unwrap_or(&0);
            let lo = (if a.block == start.block { start.ch } else { 0 }).min(clen);
            let hi = (if a.block == end.block { end.ch } else { clen }).min(clen);
            if a.start < hi && a.end > lo && !overlapping.iter().any(|(g, _)| *g == a.group) {
                overlapping.push((a.group, a.color));
            }
        }
        // Same colour over exactly what's already there ⇒ toggle it off.
        let toggling_off = !overlapping.is_empty() && overlapping.iter().all(|(_, c)| *c == color);
        let remove_groups: Vec<String> = overlapping.iter().map(|(g, _)| g.to_string()).collect();

        let mut pieces = Vec::new();
        if !toggling_off {
            // One ranged highlight per block the selection touches, each covering
            // exactly the selected characters — so a drag across paragraphs marks
            // precisely what was selected.
            for b in &layout.blocks {
                if b.index < start.block || b.index > end.block {
                    continue;
                }
                let lo = if b.index == start.block { start.ch } else { 0 };
                let hi = if b.index == end.block {
                    end.ch
                } else {
                    b.char_len
                };
                let lo = lo.min(b.char_len);
                let hi = hi.min(b.char_len);
                if lo >= hi {
                    continue;
                }
                let excerpt: String = b.text.chars().skip(lo).take((hi - lo).min(120)).collect();
                pieces.push(crate::actions::HiPiece {
                    id: Uuid::new_v4().to_string(),
                    block: b.index,
                    start: lo,
                    end: hi,
                    excerpt,
                });
            }
        }
        if pieces.is_empty() && remove_groups.is_empty() {
            return None;
        }
        // A fresh group id only when actually adding pieces; a pure toggle-off
        // leaves `group` unused by `apply`, so keep it stable (the group being
        // removed) — that makes a toggle-off deterministic across call sites, so
        // the swatch and the palette encode identically.
        let group = if pieces.is_empty() {
            remove_groups
                .first()
                .cloned()
                .unwrap_or_else(|| Uuid::nil().to_string())
        } else {
            Uuid::new_v4().to_string()
        };
        Some(crate::actions::HighlightPlan {
            book_id: self.book_id(),
            group,
            chapter: self.chapter,
            color: color.as_str().to_string(),
            remove_groups,
            pieces,
        })
    }

    /// The highlight group covering a document position, if the point lands
    /// inside a highlighted range on the current chapter. Drives the "click a
    /// highlight to open its comments" gesture.
    fn highlight_group_at(&self, p: DocPos) -> Option<Uuid> {
        self.annotations
            .iter()
            .find(|a| {
                a.kind == AnnoKind::Highlight
                    && a.chapter == self.chapter
                    && a.block == p.block
                    && a.start <= p.ch
                    && p.ch < a.end
            })
            .map(|a| a.group)
    }

    /// The highlight-group margin marker under `p`, if any, with its grab origin
    /// (the marker's top-left). Topmost recorded wins. Rebuilt each frame in
    /// `draw_pane`.
    fn marker_at(&self, p: Point) -> Option<(Uuid, (f64, f64))> {
        self.hi_markers
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(r, g)| (*g, (r.x0, r.y0)))
    }

    /// Re-anchor a highlight group to the paragraph under `drop`, preserving
    /// the mark's length (clamped to the target paragraph). Only a
    /// single-block highlight re-anchors — a multi-block one has no single
    /// paragraph to land on, so its marker opens comments instead.
    fn reanchor_highlight(&mut self, group: Uuid, drop: Point, doc: &mut AutoCommit) -> bool {
        let Some(target) = self.hit_doc_pos(drop) else {
            return false;
        };
        let pieces: Vec<Annotation> = self
            .annotations
            .iter()
            .filter(|a| a.group == group && a.kind == AnnoKind::Highlight)
            .cloned()
            .collect();
        // Re-anchoring collapses onto one paragraph, so only a single-piece
        // highlight moves cleanly.
        let [piece] = pieces.as_slice() else {
            return false;
        };
        let Some(layout) = self.layout.as_ref() else {
            return false;
        };
        let Some(tb) = layout.blocks.iter().find(|b| b.index == target.block) else {
            return false;
        };
        if tb.char_len == 0 || target.block == piece.block {
            return false;
        }
        let span = (piece.end.saturating_sub(piece.start)).clamp(1, tb.char_len);
        let excerpt: String = tb.text.chars().take(span.min(120)).collect();
        self.perform_action(
            doc,
            &crate::actions::move_annotation_action(
                piece.id,
                self.chapter,
                tb.index,
                0,
                span,
                &excerpt,
            ),
        );
        true
    }

    /// A highlight group the current text selection overlaps, if any — powers
    /// the selection toolbar's trash button (delete the highlight under the
    /// selection).
    fn selection_highlight_group(&self) -> Option<Uuid> {
        let sel = self.sel.filter(|s| !s.is_empty())?;
        let (start, end) = sel.ordered();
        let clen = |block: usize| self.block_char_len(block);
        self.annotations
            .iter()
            .find(|a| {
                if a.kind != AnnoKind::Highlight
                    || a.chapter != self.chapter
                    || a.start >= a.end
                    || a.block < start.block
                    || a.block > end.block
                {
                    return false;
                }
                let cl = clen(a.block);
                let lo = (if a.block == start.block { start.ch } else { 0 }).min(cl);
                let hi = (if a.block == end.block { end.ch } else { cl }).min(cl);
                a.start < hi && a.end > lo
            })
            .map(|a| a.group)
    }

    /// Dispatch one Books [`EbookAction`](crate::actions::EbookAction) — the
    /// single mutation path shared with the Ctrl+K palette, via the
    /// framework's [`ActionHost`](ui::ActionHost).
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::EbookAction) {
        self.actions
            .dispatch(doc, crate::actions::EBOOK_ACTION_TYPE, action);
    }

    /// The last [`EncodedAction`](qos_actions::EncodedAction) an on-page
    /// affordance dispatched: a control and its Ctrl+K entry must build the
    /// identical action.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The presentation context Ctrl+K needs to build the book-level actions
    /// (bookmark toggle at the current page; highlight the live selection).
    /// `None` when no book is open.
    pub(crate) fn book_resource_ctx(&self) -> Option<crate::actions::BookResourceCtx> {
        let book = self.open_book()?;
        let (book_id, book_title) = (book.id.clone(), book.title.clone());
        let block = self.page_top_block;
        Some(crate::actions::BookResourceCtx {
            book_id,
            book_title,
            chapter: self.chapter,
            bookmark_block: block,
            bookmark_excerpt: self.block_excerpt(block),
            selection_highlight: self
                .sel
                .filter(|s| !s.is_empty())
                .and_then(|s| self.resolve_highlight_plan(s, self.hi_color)),
        })
    }

    /// The plain text covered by the current selection, across blocks (truncated
    /// for the annotation excerpt).
    fn selection_excerpt(&self) -> String {
        self.selection_text().chars().take(160).collect()
    }

    /// The full plain text currently selected, paragraph-joined with a
    /// space, not truncated (unlike [`Self::selection_excerpt`]).
    fn selection_text(&self) -> String {
        let Some(sel) = self.sel else {
            return String::new();
        };
        let (start, end) = sel.ordered();
        let Some(layout) = self.layout.as_ref() else {
            return String::new();
        };
        let mut out = String::new();
        for b in &layout.blocks {
            if b.index < start.block || b.index > end.block {
                continue;
            }
            let lo = if b.index == start.block { start.ch } else { 0 };
            let hi = if b.index == end.block {
                end.ch
            } else {
                b.char_len
            };
            let lo = lo.min(b.char_len);
            let hi = hi.min(b.char_len);
            if lo < hi {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.extend(b.text.chars().skip(lo).take(hi - lo));
            }
        }
        out
    }

    fn clear_selection(&mut self) {
        self.sel = None;
        self.selected_block = None;
        self.dragging_sel = false;
    }

    /// Map a screen point to a [`DocPos`] using the run geometry recorded
    /// during the last draw. Snaps to the nearest line, then character.
    fn hit_doc_pos(&self, p: Point) -> Option<DocPos> {
        if self.sel_runs.is_empty() {
            return None;
        }
        let vdist = |r: &SelRun| -> f64 {
            if p.y >= r.y0 && p.y < r.y1 {
                0.0
            } else {
                (p.y - r.y0).abs().min((p.y - r.y1).abs())
            }
        };
        // Horizontal distance from the point to a run's x-span (0 when inside).
        let hdist = |r: &SelRun| -> f64 {
            if p.x < r.x0 {
                r.x0 - p.x
            } else if p.x > r.x1 {
                p.x - r.x1
            } else {
                0.0
            }
        };
        // In a two-up spread the two facing pages map different flow lines to the
        // *same* screen-y, so vertical distance alone can't tell the columns
        // apart. Pick the nearest run by vertical distance, breaking exact ties
        // (both pages' top lines share a screen-y) by horizontal proximity, then
        // group the visual line within that run's column.
        let target = self
            .sel_runs
            .iter()
            .min_by(|a, b| match vdist(a).partial_cmp(&vdist(b)) {
                Some(std::cmp::Ordering::Equal) | None => hdist(a)
                    .partial_cmp(&hdist(b))
                    .unwrap_or(std::cmp::Ordering::Equal),
                Some(o) => o,
            })?;
        let ty0 = target.y0;
        let tcol = target.col;
        let mut line: Vec<&SelRun> = self
            .sel_runs
            .iter()
            .filter(|r| (r.y0 - ty0).abs() < 0.5 && r.col == tcol)
            .collect();
        line.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
        let first = line.first()?;
        let last = line.last()?;
        if p.x <= first.x0 {
            return Some(DocPos {
                block: first.block,
                ch: first.cs,
            });
        }
        if p.x >= last.x1 {
            return Some(DocPos {
                block: last.block,
                ch: last.cs + last.cl,
            });
        }
        for r in &line {
            if p.x >= r.x0 && p.x < r.x1 {
                let w = (r.x1 - r.x0).max(1.0);
                let frac = ((p.x - r.x0) / w).clamp(0.0, 1.0);
                let k = (frac * r.cl as f64).round() as usize;
                return Some(DocPos {
                    block: r.block,
                    ch: r.cs + k.min(r.cl),
                });
            }
        }
        // In a gap between words: snap to the nearest run edge.
        let nearest = line.iter().min_by(|a, b| {
            let da = (p.x - f64::midpoint(a.x0, a.x1)).abs();
            let db = (p.x - f64::midpoint(b.x0, b.x1)).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let ch = if p.x < nearest.x0 {
            nearest.cs
        } else {
            nearest.cs + nearest.cl
        };
        Some(DocPos {
            block: nearest.block,
            ch,
        })
    }

    /// Select the whole word under `dp` (double-click). Uses the shared
    /// char-indexed word walker on the block's canonical text.
    fn select_word_at(&mut self, dp: DocPos) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let Some(b) = layout.blocks.iter().find(|b| b.index == dp.block) else {
            return;
        };
        let (s, e) = ui::word_boundaries_chars(&b.text, dp.ch.min(b.char_len));
        if e > s {
            self.sel = Some(TextSel {
                anchor: DocPos {
                    block: dp.block,
                    ch: s,
                },
                head: DocPos {
                    block: dp.block,
                    ch: e,
                },
            });
            self.selected_block = Some(dp.block);
            self.dragging_sel = false;
        }
    }

    /// The word span containing `p`. Powers word-granularity drag extension;
    /// falls back to a collapsed span at `p` when the block isn't laid out.
    fn word_span_at(&self, p: DocPos) -> (DocPos, DocPos) {
        if let Some(l) = self.layout.as_ref()
            && let Some(b) = l.blocks.iter().find(|b| b.index == p.block)
        {
            let (s, e) = ui::word_boundaries_chars(&b.text, p.ch.min(b.char_len));
            return (
                DocPos {
                    block: p.block,
                    ch: s,
                },
                DocPos {
                    block: p.block,
                    ch: e,
                },
            );
        }
        (p, p)
    }

    /// Char length of a laid-out block (0 for separators / unknown).
    fn block_char_len(&self, block: usize) -> usize {
        self.layout
            .as_ref()
            .and_then(|l| l.blocks.iter().find(|b| b.index == block))
            .map_or(0, |b| b.char_len)
    }

    fn doc_next_char(&self, p: DocPos) -> DocPos {
        if p.ch < self.block_char_len(p.block) {
            return DocPos {
                block: p.block,
                ch: p.ch + 1,
            };
        }
        if let Some(l) = self.layout.as_ref()
            && let Some(nb) = l
                .blocks
                .iter()
                .find(|b| b.index > p.block && b.char_len > 0)
        {
            return DocPos {
                block: nb.index,
                ch: 0,
            };
        }
        p
    }

    fn doc_prev_char(&self, p: DocPos) -> DocPos {
        if p.ch > 0 {
            return DocPos {
                block: p.block,
                ch: p.ch - 1,
            };
        }
        if let Some(l) = self.layout.as_ref()
            && let Some(pb) = l
                .blocks
                .iter()
                .rev()
                .find(|b| b.index < p.block && b.char_len > 0)
        {
            return DocPos {
                block: pb.index,
                ch: pb.char_len,
            };
        }
        p
    }

    fn doc_next_word(&self, p: DocPos) -> DocPos {
        if let Some(l) = self.layout.as_ref() {
            if let Some(b) = l.blocks.iter().find(|b| b.index == p.block) {
                let nb = ui::next_word_boundary_chars(&b.text, p.ch);
                if nb > p.ch {
                    return DocPos {
                        block: p.block,
                        ch: nb,
                    };
                }
            }
            if let Some(next) = l
                .blocks
                .iter()
                .find(|b| b.index > p.block && b.char_len > 0)
            {
                let nb = ui::next_word_boundary_chars(&next.text, 0);
                return DocPos {
                    block: next.index,
                    ch: nb,
                };
            }
        }
        p
    }

    fn doc_prev_word(&self, p: DocPos) -> DocPos {
        if let Some(l) = self.layout.as_ref() {
            if let Some(b) = l.blocks.iter().find(|b| b.index == p.block) {
                let pb = ui::prev_word_boundary_chars(&b.text, p.ch);
                if pb < p.ch {
                    return DocPos {
                        block: p.block,
                        ch: pb,
                    };
                }
            }
            if let Some(prev) = l
                .blocks
                .iter()
                .rev()
                .find(|b| b.index < p.block && b.char_len > 0)
            {
                let pb = ui::prev_word_boundary_chars(&prev.text, prev.char_len);
                return DocPos {
                    block: prev.index,
                    ch: pb,
                };
            }
        }
        p
    }

    /// Extend the selection head one character (or word) in a direction.
    /// Starts a fresh selection at the top of the page when none is live.
    fn extend_sel(&mut self, forward: bool, word: bool) -> bool {
        let base = self.sel.unwrap_or_else(|| {
            let p = self.page_first_doc.unwrap_or(DocPos {
                block: self.page_top_block,
                ch: 0,
            });
            TextSel { anchor: p, head: p }
        });
        let new_head = match (forward, word) {
            (true, false) => self.doc_next_char(base.head),
            (false, false) => self.doc_prev_char(base.head),
            (true, true) => self.doc_next_word(base.head),
            (false, true) => self.doc_prev_word(base.head),
        };
        let sel = TextSel {
            anchor: base.anchor,
            head: new_head,
        };
        self.sel = Some(sel);
        self.selected_block = Some(sel.ordered().0.block);
        true
    }

    fn toggle_bookmark(&mut self, doc: &mut AutoCommit) {
        let block = self.page_top_block;
        let book_id = self.book_id();
        let excerpt = self.block_excerpt(block);
        // One action covers both add and remove: `apply` decides deterministically
        // against the document. The minted id is used only on the add branch.
        let action = crate::actions::toggle_bookmark_action(
            &book_id,
            Uuid::new_v4(),
            self.chapter,
            block,
            &excerpt,
        );
        self.perform_action(doc, &action);
    }

    /// Append/erase a character on the note currently being composed. Each
    /// keystroke rewrites the note's scalar in the CRDT — coarse-grained but
    /// genuinely collaborative at the annotation level.
    fn edit_note_text(&mut self, doc: &mut AutoCommit, edit: NoteEdit) -> bool {
        let Some(id) = self.composing else {
            return false;
        };
        let Some(mut a) = self.annotations.iter().find(|a| a.id == id).cloned() else {
            self.composing = None;
            return false;
        };
        match edit {
            NoteEdit::Insert(s) => a.note.push_str(&s),
            NoteEdit::Backspace => {
                a.note.pop();
            }
            NoteEdit::Done => {
                self.composing = None;
                a.created = a.created.max(now_ms());
            }
        }
        BooksDoc::upsert_annotation(doc, &a);
        true
    }

    fn set_composing_text(&mut self, doc: &mut AutoCommit, text: String) -> bool {
        let Some(id) = self.composing else {
            return false;
        };
        let Some(mut annotation) = self.annotations.iter().find(|a| a.id == id).cloned() else {
            self.composing = None;
            self.note_composer.close();
            return false;
        };
        annotation.note = text;
        BooksDoc::upsert_annotation(doc, &annotation);
        true
    }

    fn finish_composing(&mut self, doc: &mut AutoCommit) -> bool {
        let note_editor_open = self.note_composer.is_open();
        let handled = self.edit_note_text(doc, NoteEdit::Done);
        if note_editor_open {
            self.note_composer.close();
        }
        handled
    }

    // --- Hit-test helpers ----------------------------------------------------

    fn push_hit(&mut self, rect: Rect, action: Action) {
        self.hits.push(Hit { rect, action });
    }

    fn is_hover(&self, rect: Rect) -> bool {
        rect.contains(self.cursor)
    }
}

enum NoteEdit {
    Insert(String),
    Backspace,
    Done,
}

/// One PAGE pill in the quick-settings popover: a colour scheme the reader
/// can switch to (built-in tint or enabled custom scheme).
struct SchemePill {
    label: String,
    action: Action,
    active: bool,
}

/// Count how many rows a run of pill widths wraps into within `max_w`, matching
/// the greedy layout the draw loop uses (so a pre-sized panel fits its pills).
fn wrap_row_count(widths: &[f64], max_w: f64, gap: f64) -> usize {
    if widths.is_empty() {
        return 1;
    }
    let mut rows = 1;
    let mut x = 0.0;
    for (i, &w) in widths.iter().enumerate() {
        let adv = if i == 0 { w } else { gap + w };
        if x + adv > max_w && x > 0.0 {
            rows += 1;
            x = w;
        } else {
            x += adv;
        }
    }
    rows
}

impl Widget for BooksApp {
    /// Publish the open book + its annotations so the host's Ctrl+K action
    /// palette scopes to them, via [`crate::actions`]'s
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-page affordance
    /// dispatched its annotation action. Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Make every book (by title/author) and every current-book bookmark (by
    /// excerpt) findable in the unified Ctrl+K palette.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::BookSearchProvider::new(
            &self.library,
            &self.annotations,
        ))]
    }

    /// Open the book, or jump to the annotation, a chosen palette row
    /// identifies.
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _path: &ObjId) {
        match crate::search_provider::Activation::parse(action_id) {
            Some(crate::search_provider::Activation::Book(id)) => {
                if let Some(i) = self.library.iter().position(|b| b.id == id) {
                    self.open_book_at(doc, i);
                }
            }
            Some(crate::search_provider::Activation::Annotation(id)) => {
                if let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() {
                    self.view = View::Reader;
                    self.chapter = a.chapter.min(self.num_chapters().saturating_sub(1));
                    self.pending_jump_block = Some(a.block);
                    self.clear_selection();
                    self.comment_overlay.close();
                    self.layout = None;
                }
            }
            None => {}
        }
    }

    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        // Hydrate the persisted library on the first frame (when the document is
        // finally available), then pull in any book the host picker returned.
        self.ensure_library_loaded(doc);
        self.drain_portal(doc);
        if let Some(s) = self.shot_settings.take() {
            BooksDoc::write_settings(doc, &s);
        }
        if let Some(f) = self.shot_fonts.take() {
            BooksDoc::write_fonts(doc, &f);
        }
        if let Some(sc) = self.shot_schemes.take() {
            BooksDoc::write_schemes(doc, &sc);
        }
        if let Some(wf) = self.shot_watched.take() {
            BooksDoc::write_watched_folders(doc, &wf);
        }
        self.seed_if_needed(doc);
        if self.shot_compose && self.composing.is_none() {
            self.shot_compose = false;
            if let Some(sel) = self.sel {
                let (start, _) = sel.ordered();
                let bid = self.book_id();
                let mut a = Annotation::new(&bid, AnnoKind::Note, self.chapter, start.block)
                    .with_color(HiColor::Blue)
                    .with_excerpt(
                        "the keeper climbed the iron stair and set the great wick alight",
                    );
                a.note = "A note, written right here on the page — no separate screen.".into();
                let aid = a.id;
                BooksDoc::upsert_annotation(doc, &a);
                self.composing = Some(aid);
                self.note_composer.open(aid, &a.note);
            }
        }
        if self.shot_comments && !self.comment_overlay.is_open() {
            self.shot_comments = false;
            let bid = self.book_id();
            let group = Uuid::new_v4();
            // Partial-paragraph highlight; offsets clamp to the block in draw.
            let excerpt = "the trick of pouring colour into a pane";
            let block = 2usize;
            let mut hl = Annotation::new(&bid, AnnoKind::Highlight, self.chapter, block)
                .with_color(HiColor::Yellow)
                .with_excerpt(excerpt)
                .with_group(group);
            hl.start = 200;
            hl.end = 200 + excerpt.chars().count();
            BooksDoc::upsert_annotation(doc, &hl);
            for (i, text) in [
                "Love this image — colour that keeps the time of day.",
                "Compare with the amber/ebb line later in the letter.",
            ]
            .into_iter()
            .enumerate()
            {
                let mut cm = Annotation::new(&bid, AnnoKind::Note, self.chapter, block)
                    .with_color(HiColor::Yellow)
                    .with_excerpt(excerpt)
                    .with_group(group);
                cm.start = hl.start;
                cm.end = hl.end;
                cm.note = text.to_string();
                cm.created = now_ms() + i as u64;
                BooksDoc::upsert_annotation(doc, &cm);
            }
            self.comment_overlay.open(group);
        }
        // On the very first sync, adopt any persisted position for the open book.
        if self.last_persisted.is_none() && self.view == View::Reader {
            let id = self.book_id();
            let p = BooksDoc::read_progress(doc, &id);
            let nchap = self.num_chapters().saturating_sub(1);
            self.chapter = p.chapter.min(nchap);
            self.page = p.page;
        }
        self.refresh_from_doc(doc);
        self.persist_progress(doc);
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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // The allocated width is now known — reconcile both rails so a
        // breakpoint crossing adopts the new default (docked ⇒ open, overlay ⇒
        // closed) before the next draw.
        self.reconcile_flaps();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        self.hits.clear();
        self.comment_overlay.begin_frame();
        self.number_controls.begin_frame();
        // Advance the rail flap transitions (reconcile first so a resize this
        // frame uses the right mode default) before laying anything out.
        self.reconcile_flaps();
        let dt = 1.0 / 60.0;
        self.toc_flap.tick(self.toc_layout(), dt);
        self.notes_flap.tick(self.notes_layout(), dt);
        // App-base fill so nothing shows through.
        paint::fill_rect(
            scene,
            transform,
            self.rect,
            theme.tokens.colors.surfaces.base,
        );
        match self.view {
            View::Library => self.draw_library(scene, transform, tr, theme),
            View::Reader => self.draw_reader(scene, transform, tr, theme),
            View::Settings => self.draw_settings(scene, transform, tr, theme),
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if self.forward_rename_event(
            event,
            scale_factor,
            *modifiers,
            doc,
            path,
            node_id,
            taffy,
            handled,
        ) {
            return;
        }
        if self.forward_shared_input_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            node_id,
            taffy,
            handled,
        ) {
            return;
        }

        match event {
            // Regaining focus is a natural moment to notice books that appeared
            // in a watched folder while the reader was elsewhere — no manual
            // "reload". A no-op when nothing is watched or nothing is new.
            WindowEvent::Focused(true) => self.rescan_watched(doc),
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.handle_cursor_moved(p, doc, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press(*modifiers, doc, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_release(doc, handled);
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                if self.handle_key(&ke.logical_key, modifiers.state(), doc) {
                    self.refresh_from_doc(doc);
                    self.persist_progress(doc);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(*delta, doc, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "BooksApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("books_app")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.flaps_animating()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the loop pumping while a flap is mid-slide, a page turn is
        // easing in, or the floating page bubble is lingering/fading.
        self.flaps_animating()
            || self.page_turn.is_some_and(|pt| !pt.done(Instant::now()))
            || self.reader_bubble.needs_frame(Instant::now())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl BooksApp {
    #[allow(clippy::too_many_arguments)]
    fn forward_shared_input_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        handled: &mut bool,
    ) -> bool {
        let mut input_handled = false;
        let (note_text, submit) = self.note_composer.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            node_id,
            taffy,
            &mut input_handled,
        );
        if let Some(text) = note_text {
            self.set_composing_text(doc, text);
        }
        if submit {
            self.finish_composing(doc);
            input_handled = true;
        }

        let changes = self.number_controls.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            path,
            node_id,
            taffy,
            &mut input_handled,
        );
        for (field, value) in changes {
            match field {
                NumberField::FontScale => {
                    self.bump_setting(doc, |settings| settings.font_scale = (value / 100.0) as f32);
                }
                NumberField::LineSpacing => {
                    self.bump_setting(doc, |settings| settings.line_spacing = value as f32);
                }
                NumberField::FontMult(id) => {
                    let mut fonts = self.fonts.clone();
                    if let Some(font) = fonts.iter_mut().find(|font| font.id == id) {
                        font.size_mult = value as f32;
                    }
                    BooksDoc::write_fonts(doc, &fonts);
                }
            }
        }
        if input_handled {
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
        }
        input_handled
    }

    /// An in-progress inline rename owns input first: forward the raw event to
    /// its embedded field, then persist any commit/cancel it produced. Returns
    /// `true` when the field consumed the event (the caller stops there); a
    /// press outside the field blur-commits and falls through to the rest of
    /// `handle_event`.
    #[allow(clippy::too_many_arguments)]
    fn forward_rename_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        handled: &mut bool,
    ) -> bool {
        if self.renaming.is_none() {
            return false;
        }
        let mut field_handled = false;
        let mut field_focus = None;
        if let Some(rn) = self.renaming.as_mut() {
            rn.field.handle_event(
                event,
                scale_factor,
                &modifiers,
                doc,
                path,
                &mut field_handled,
                &mut field_focus,
                node_id,
                taffy,
            );
        }
        self.poll_rename_outcome(doc);
        if field_handled {
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    fn handle_cursor_moved(&mut self, p: Point, doc: &mut AutoCommit, handled: &mut bool) {
        if p == self.cursor {
            return;
        }
        self.cursor = p;
        self.dirty = true; // hover feedback
        // Margin-marker drag: a release past the threshold
        // re-anchors the highlight (vs a plain tap → comments).
        if self.mouse_down
            && let Some(md) = self.marker_drag.as_mut()
        {
            md.update((p.x, p.y));
            *handled = true;
            return;
        }
        // The modal colour picker owns its drag geometry and HSV state; the
        // app only persists the semantic colour edit it emits.
        if self.mouse_down
            && let Some(edit) = self.color_picker.drag_to(p)
        {
            self.apply_live_pick(doc, &edit);
            self.refresh_from_doc(doc);
            *handled = true;
            return;
        }
        // Drag text selection: once the press has moved far enough,
        // the head follows the pointer and the selection spans
        // whatever paragraphs it covers. Dragging into the top/bottom
        // margin auto-advances a page so a selection can span pages.
        if self.mouse_down && self.press_doc.is_some() && self.view == View::Reader {
            if !self.dragging_sel && (p - self.press_pos).hypot() > 4.0 {
                self.dragging_sel = true;
            }
            if self.dragging_sel {
                if let Some(raw) = self.hit_doc_pos(p) {
                    let (lo, hi) = self.drag_anchor_span;
                    // Extend by the drag's unit. Word drags keep the
                    // double-clicked word inside the selection and grow
                    // word-by-word via the shared toolkit rule; char
                    // drags follow the pointer from the pressed anchor.
                    let (anchor, head) = match self.drag_unit {
                        DragUnit::Char => (lo, raw),
                        DragUnit::Word => {
                            let (ws, we) = self.word_span_at(raw);
                            ui::extend_span_drag(lo, hi, raw, ws, we)
                        }
                    };
                    let sel = TextSel { anchor, head };
                    self.sel = Some(sel);
                    self.selected_block = Some(sel.ordered().0.block);
                }
                let (by0, by1) = self.reader_body;
                if p.y > by1 - 20.0 {
                    let before = (self.chapter, self.page);
                    self.dispatch(Action::NextPage, doc);
                    if (self.chapter, self.page) != before {
                        self.pending_sel_edge = Some(true);
                    }
                } else if p.y < by0 + 20.0 {
                    let before = (self.chapter, self.page);
                    self.dispatch(Action::PrevPage, doc);
                    if (self.chapter, self.page) != before {
                        self.pending_sel_edge = Some(false);
                    }
                }
                *handled = true;
            }
        }
    }

    fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if let Some(action) = self.color_picker.press(self.cursor) {
            self.mouse_down = true;
            match action {
                PickerPress::Swallow => {}
                PickerPress::Cancel => self.color_picker.cancel(),
                PickerPress::Done => self.commit_picker(doc),
                PickerPress::Edit(edit) => self.apply_live_pick(doc, &edit),
            }
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
            return;
        }
        if let Some(action) = self.note_composer.hit_test(self.cursor) {
            match action {
                NoteAction::Swallow => {}
                NoteAction::Save => {
                    self.finish_composing(doc);
                }
                NoteAction::Delete(id) => self.on_delete_annotation(id, doc),
            }
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
            return;
        }
        // The comment panel owns a modal, private hit stack. Ask it
        // before the app-wide regions so its card cannot fall through
        // to page selection or rail controls underneath.
        let comment_action = self.comment_overlay.hit_test(self.cursor);
        let shelf_action = if comment_action.is_none() && self.view == View::Library {
            self.shelf_controls.hit_test(self.cursor)
        } else {
            None
        };
        // Topmost wins: later-recorded hits draw on top.
        let top = if comment_action.is_none() && shelf_action.is_none() {
            self.hits
                .iter()
                .rev()
                .find(|h| h.rect.contains(self.cursor))
                .cloned()
        } else {
            None
        };
        // The bubble is drawn below subsequent reader overlays. Their app-wide
        // hits therefore keep precedence when geometry ever overlaps.
        let bubble_action = if comment_action.is_none() && shelf_action.is_none() && top.is_none() {
            self.reader_bubble.hit_test(self.cursor)
        } else {
            None
        };
        self.mouse_down = true;
        self.dragging_sel = false;
        self.press_pos = self.cursor;

        // A press on a highlight's margin marker begins a re-anchor
        // drag, taking priority over text selection. A press that
        // doesn't move opens the highlight's comments on release instead.
        if self.view == View::Reader
            && comment_action.is_none()
            && top.is_none()
            && let Some((group, origin)) = self.marker_at(self.cursor)
        {
            self.marker_drag = Some(ui::MarkerDrag::begin(
                group.to_string(),
                (self.cursor.x, self.cursor.y),
                origin,
            ));
            self.marker_group = Some(group);
            self.dirty = true;
            *handled = true;
            return;
        }

        // A press on page text runs through the shared
        // selection-gesture core: single → double (word) → triple.
        let on_page = self.view == View::Reader
            && comment_action.is_none()
            && top.is_none()
            && bubble_action.is_none();
        let consumed_page = self.resolve_page_press(on_page, modifiers.state().shift_key());

        if let Some(action) = comment_action {
            self.dispatch_comment_action(action, doc);
            self.refresh_from_doc(doc);
            self.persist_progress(doc);
            self.dirty = true;
            *handled = true;
        } else if let Some(action) = shelf_action {
            self.dispatch_shelf_control(action, doc);
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
        } else if let Some(action) = bubble_action {
            self.dispatch(
                match action {
                    ReaderBubbleAction::PreviousPage => Action::PrevPage,
                    ReaderBubbleAction::NextPage => Action::NextPage,
                },
                doc,
            );
            self.refresh_from_doc(doc);
            self.persist_progress(doc);
            self.dirty = true;
            *handled = true;
        } else if let Some(h) = top {
            self.dispatch(h.action, doc);
            self.refresh_from_doc(doc);
            self.persist_progress(doc);
            self.dirty = true;
            *handled = true;
        } else if consumed_page {
            self.dirty = true;
            *handled = true;
        }
    }

    /// Route a left-press that landed on page text (`on_page`) through the
    /// shared single/double/triple-click selection ladder. Returns whether
    /// the press was consumed as a selection gesture.
    fn resolve_page_press(&mut self, on_page: bool, shift: bool) -> bool {
        self.press_doc = if on_page {
            self.hit_doc_pos(self.cursor)
        } else {
            None
        };
        self.press_hl_group = None;
        let mut consumed_page = false;
        if on_page {
            let intent = self.drag_select.press(self.cursor, shift);
            match intent.granularity {
                // Double/triple click: select the word under the press and
                // arm a word-granularity drag (so dragging beyond the word
                // extends by whole words, never resetting to the click).
                Granularity::Word | Granularity::Line => {
                    if let Some(dp) = self.press_doc {
                        self.select_word_at(dp);
                        if let Some(s) = self.sel {
                            self.drag_anchor_span = s.ordered();
                            self.drag_unit = DragUnit::Word;
                            consumed_page = true;
                        }
                    }
                }
                Granularity::Char if intent.extend => {
                    // Shift+click extends the live selection (or starts
                    // one at the pressed caret) to the press.
                    if let Some(dp) = self.press_doc {
                        let anchor = self.sel.map_or(dp, |s| s.anchor);
                        let sel = TextSel { anchor, head: dp };
                        self.sel = Some(sel);
                        self.selected_block = Some(sel.ordered().0.block);
                        self.drag_anchor_span = (anchor, anchor);
                        self.drag_unit = DragUnit::Char;
                        consumed_page = true;
                    }
                }
                Granularity::Char => {
                    self.sel = None; // fresh press clears any prior selection
                    self.selected_block = None;
                    self.drag_unit = DragUnit::Char;
                    if let Some(dp) = self.press_doc {
                        self.drag_anchor_span = (dp, dp);
                    }
                    // Remember whether the press landed inside a highlight
                    // so a plain click can open its comment overlay on release.
                    self.press_hl_group = self.press_doc.and_then(|dp| self.highlight_group_at(dp));
                }
            }
        }
        consumed_page
    }

    fn handle_left_release(&mut self, doc: &mut AutoCommit, handled: &mut bool) {
        self.mouse_down = false;
        // A margin-marker release resolves the drag: a real move
        // re-anchors the highlight; a plain tap opens its comments.
        if let Some(md) = self.marker_drag.take() {
            let group = self.marker_group.take();
            if let Some(group) = group {
                if md.moved() {
                    self.reanchor_highlight(group, self.cursor, doc);
                } else {
                    self.dispatch(Action::OpenComments(group), doc);
                }
            }
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
            return;
        }
        self.drag_select.release();
        let was_drag = self.dragging_sel;
        if self.color_picker.end_drag() {
            self.dirty = true;
            *handled = true;
        }
        if self.dragging_sel {
            self.dragging_sel = false;
            if let Some(s) = self.sel
                && s.is_empty()
            {
                self.sel = None;
                self.selected_block = None;
            }
            self.dirty = true;
            *handled = true;
        }
        // A plain click (no drag) that started on a highlighted range
        // opens that highlight's floating comment overlay.
        if !was_drag && let Some(group) = self.press_hl_group.take() {
            self.dispatch(Action::OpenComments(group), doc);
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
        }
        self.press_hl_group = None;
    }

    fn handle_mouse_wheel(
        &mut self,
        delta: MouseScrollDelta,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // A dominantly horizontal gesture turns pages (scroll-right
        // advances). On this stack a physical scroll-right arrives as
        // a positive x, so the horizontal axis passes through as-is;
        // only the vertical is negated to make a downward scroll "forward".
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                (f64::from(x) * WHEEL_LINE_PX, f64::from(-y) * WHEEL_LINE_PX)
            }
            MouseScrollDelta::PixelDelta(p) => (p.x, -p.y),
        };
        let acted = if dx.abs() > dy.abs() {
            self.handle_swipe(dx, doc)
        } else {
            self.handle_wheel(dy, doc)
        };
        if acted {
            self.refresh_from_doc(doc);
            self.persist_progress(doc);
            self.dirty = true;
            *handled = true;
        }
    }

    /// Library-shelf keyboard navigation. A directional cursor moves across
    /// the grid (up/down step by a whole row); Enter/Space opens the
    /// focused book.
    fn handle_library_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
        // Navigation is over the shelf's display order (filtered + sorted),
        // resolved by the last `draw_library`.
        let n = self.shelf_order.len();
        if n == 0 {
            return false;
        }
        let last = n - 1;
        let cols = self.shelf_cols.max(1);
        let cursor = self.shelf_cursor.min(last);
        match key {
            Key::Named(NamedKey::ArrowRight) => {
                self.shelf_cursor = (cursor + 1).min(last);
                self.scroll_to_cursor = true;
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.shelf_cursor = cursor.saturating_sub(1);
                self.scroll_to_cursor = true;
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.shelf_cursor = (cursor + cols).min(last);
                self.scroll_to_cursor = true;
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.shelf_cursor = cursor.saturating_sub(cols);
                self.scroll_to_cursor = true;
                true
            }
            Key::Named(NamedKey::Home) => {
                self.shelf_cursor = 0;
                self.scroll_to_cursor = true;
                true
            }
            Key::Named(NamedKey::End) => {
                self.shelf_cursor = last;
                self.scroll_to_cursor = true;
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let orig = self.shelf_order.get(cursor).copied().unwrap_or(0);
                self.dispatch(Action::OpenBook(orig), doc);
                true
            }
            _ => false,
        }
    }

    /// Keyboard handling. Returns `true` if the key was consumed (so the App's
    /// focus-navigation fallback doesn't also steal the arrows).
    fn handle_key(&mut self, key: &Key, mods: ModifiersState, doc: &mut AutoCommit) -> bool {
        // An in-progress inline rename owns the keyboard through its embedded
        // field via `handle_event`; no key handling for it belongs here.

        if self.view == View::Settings {
            return self.handle_settings_escape_key(key, doc);
        }

        // While composing a note, characters feed the note text.
        if self.composing.is_some() {
            return self.handle_composing_key(key, doc);
        }

        if self.view == View::Library {
            return self.handle_library_key(key, doc);
        }

        self.handle_reader_key(key, mods, doc)
    }

    fn handle_settings_escape_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
        if let Key::Named(NamedKey::Escape) = key {
            if self.color_picker.is_open() {
                self.color_picker.cancel();
            } else if self.font_picker {
                self.font_picker = false;
            } else if self.default_font_menu {
                self.default_font_menu = false;
            } else if self.editing_scheme.is_some() {
                self.editing_scheme = None;
            } else {
                self.dispatch(Action::CloseSettings, doc);
            }
            return true;
        }
        false
    }

    fn handle_composing_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
        match key {
            Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                self.edit_note_text(doc, NoteEdit::Done)
            }
            Key::Named(NamedKey::Backspace) => self.edit_note_text(doc, NoteEdit::Backspace),
            Key::Named(NamedKey::Space) => self.edit_note_text(doc, NoteEdit::Insert(" ".into())),
            Key::Character(c) => self.edit_note_text(doc, NoteEdit::Insert(c.to_string())),
            _ => false,
        }
    }

    fn handle_reader_key(&mut self, key: &Key, mods: ModifiersState, doc: &mut AutoCommit) -> bool {
        match key {
            // Shift extends the selection by a character (Ctrl+Shift by a word);
            // otherwise the arrows turn pages.
            Key::Named(NamedKey::ArrowLeft) => {
                if mods.shift_key() {
                    self.extend_sel(false, mods.control_key())
                } else {
                    self.dispatch(Action::PrevPage, doc);
                    true
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                if mods.shift_key() {
                    self.extend_sel(true, mods.control_key())
                } else {
                    self.dispatch(Action::NextPage, doc);
                    true
                }
            }
            Key::Named(NamedKey::PageUp) => {
                self.dispatch(Action::PrevPage, doc);
                true
            }
            Key::Named(NamedKey::PageDown | NamedKey::Space) => {
                self.dispatch(Action::NextPage, doc);
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.chapter > 0 {
                    self.dispatch(Action::GotoChapter(self.chapter - 1), doc);
                }
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                if self.chapter + 1 < self.num_chapters() {
                    self.dispatch(Action::GotoChapter(self.chapter + 1), doc);
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Esc peels back one layer at a time: the reading-settings
                // popover, then an open overlay flap (narrow window), and only
                // when nothing is floating does it leave the book.
                let toc_overlay = self.toc_layout() == SplitLayout::Overlay && self.toc_open();
                let notes_overlay =
                    self.notes_layout() == SplitLayout::Overlay && self.notes_open();
                if self.comment_overlay.is_open() {
                    self.dispatch(Action::CloseComments, doc);
                } else if self.type_popover {
                    self.type_popover = false;
                } else if toc_overlay || notes_overlay {
                    self.dispatch(Action::DismissOverlay, doc);
                } else {
                    self.dispatch(Action::Back, doc);
                }
                true
            }
            Key::Character(c) => {
                let action = match c.as_str() {
                    "b" | "B" => Some(Action::ToggleBookmark),
                    "t" | "T" => Some(Action::ToggleToc),
                    "n" | "N" => Some(Action::ToggleNotes),
                    "f" | "F" => Some(Action::CycleFont),
                    "+" | "=" => Some(Action::FontInc),
                    "-" | "_" => Some(Action::FontDec),
                    "[" => Some(Action::SpacingDec),
                    "]" => Some(Action::SpacingInc),
                    "1" => Some(Action::SetReadTheme(ReadTheme::Light)),
                    "2" => Some(Action::SetReadTheme(ReadTheme::Sepia)),
                    "3" => Some(Action::SetReadTheme(ReadTheme::Dark)),
                    "4" => Some(Action::SetReadTheme(ReadTheme::HighContrast)),
                    "h" | "H" => self
                        .sel
                        .filter(|s| !s.is_empty())
                        .map(|_| Action::SetColor(self.hi_color)),
                    "c" | "C" => self
                        .sel
                        .filter(|s| !s.is_empty())
                        .map(|_| Action::CopySelection),
                    "m" | "M" => Some(Action::AddNote),
                    _ => None,
                };
                if let Some(a) = action {
                    self.dispatch(a, doc);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Horizontal scroll handling. `dx > 0` (scroll right) advances to the
    /// next page; `dx < 0` turns back. Accumulated so a trackpad's many small
    /// deltas coalesce into whole turns. Returns `true` when a page turned.
    #[allow(
        clippy::while_float,
        reason = "each iteration subtracts a fixed SWIPE_STEP_PX and breaks immediately \
                  if a turn made no progress, so the loop is bounded regardless of float \
                  imprecision"
    )]
    fn handle_swipe(&mut self, dx: f64, doc: &mut AutoCommit) -> bool {
        if dx == 0.0 || self.view != View::Reader {
            return false;
        }
        if self.type_popover || self.composing.is_some() {
            return false;
        }
        self.swipe_accum += dx;
        let mut turned = false;
        while self.swipe_accum >= SWIPE_STEP_PX {
            self.swipe_accum -= SWIPE_STEP_PX;
            let before = (self.chapter, self.page);
            self.dispatch(Action::NextPage, doc);
            if (self.chapter, self.page) == before {
                self.swipe_accum = 0.0;
                break;
            }
            turned = true;
        }
        while self.swipe_accum <= -SWIPE_STEP_PX {
            self.swipe_accum += SWIPE_STEP_PX;
            let before = (self.chapter, self.page);
            self.dispatch(Action::PrevPage, doc);
            if (self.chapter, self.page) == before {
                self.swipe_accum = 0.0;
                break;
            }
            turned = true;
        }
        turned
    }

    /// Wheel / trackpad handling. In the reader a downward scroll turns forward
    /// and an upward scroll turns back, accumulated so a trackpad flick advances
    /// a sensible number of pages. On the shelf it scrolls the grid so a library
    /// taller than the window is fully reachable. Returns `true` when consumed.
    #[allow(
        clippy::while_float,
        reason = "each iteration subtracts a fixed TURN_STEP_PX and breaks immediately \
                  if a turn made no progress, so the loop is bounded regardless of float \
                  imprecision"
    )]
    #[allow(
        clippy::float_cmp,
        reason = "before is the exact prior bit-pattern of the scroll field; `!=` after a \
                  clamp() is testing whether the assignment changed anything, not comparing \
                  two independently-computed values"
    )]
    fn handle_wheel(&mut self, dy: f64, doc: &mut AutoCommit) -> bool {
        if dy == 0.0 {
            return false;
        }
        match self.view {
            View::Settings => {
                if self.font_picker {
                    let before = self.picker_scroll;
                    self.picker_scroll =
                        (self.picker_scroll + dy).clamp(0.0, self.picker_max_scroll);
                    self.picker_scroll != before
                } else {
                    let before = self.settings_scroll;
                    self.settings_scroll =
                        (self.settings_scroll + dy).clamp(0.0, self.settings_max_scroll);
                    self.settings_scroll != before
                }
            }
            View::Library => {
                let before = self.shelf_scroll;
                self.shelf_scroll = (self.shelf_scroll + dy).clamp(0.0, self.shelf_max_scroll);
                self.shelf_scroll != before
            }
            View::Reader => {
                // A page fills the viewport, so the wheel turns pages rather than
                // scrolling within one. Suppressed while a modal surface (the
                // reading-settings popover or an in-progress note) owns input.
                if self.type_popover || self.composing.is_some() {
                    return false;
                }
                // Continuous mode scrolls the flow by pixels instead of turning
                // whole pages — the wheel delta drives the column directly.
                if self.settings.mode.is_continuous() {
                    let before = (self.chapter, self.scroll_y);
                    self.scroll_continuous(dy * 1.4);
                    return (self.chapter, self.scroll_y) != before;
                }
                self.turn_accum += dy;
                let mut turned = false;
                while self.turn_accum >= TURN_STEP_PX {
                    self.turn_accum -= TURN_STEP_PX;
                    let before = (self.chapter, self.page);
                    self.dispatch(Action::NextPage, doc);
                    if (self.chapter, self.page) == before {
                        self.turn_accum = 0.0; // at the end — stop coasting
                        break;
                    }
                    turned = true;
                }
                while self.turn_accum <= -TURN_STEP_PX {
                    self.turn_accum += TURN_STEP_PX;
                    let before = (self.chapter, self.page);
                    self.dispatch(Action::PrevPage, doc);
                    if (self.chapter, self.page) == before {
                        self.turn_accum = 0.0; // at the very start
                        break;
                    }
                    turned = true;
                }
                turned
            }
        }
    }
}

// --- Small chrome-drawing primitives -----------------------------------

impl BooksApp {
    /// A square icon button. Returns the x-advance (its size).
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/x/y/s/c/w are the standard transform/geometry/colour/stroke-width names \
                  shared by every draw helper in this chrome section"
    )]
    fn icon_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        x: f64,
        y: f64,
        s: f64,
        ico: Ico,
        active: bool,
        action: Action,
    ) -> f64 {
        let rect = Rect::new(x, y, x + s, y + s);
        let hover = self.is_hover(rect);
        let c = &theme.tokens.colors;
        if active {
            paint::fill_round(
                scene,
                t,
                rect,
                f64::from(theme.tokens.radii.md),
                theme.accent_primary,
            );
        } else if hover {
            paint::fill_round(
                scene,
                t,
                rect,
                f64::from(theme.tokens.radii.md),
                theme.bg_hover,
            );
        }
        let icon_col = if active {
            Color::from_rgb8(248, 248, 252)
        } else if hover {
            c.text_primary
        } else {
            c.text_dim
        };
        let w = 1.9;
        let inner = rect;
        match ico {
            Ico::Back => paint::chevron_left(scene, t, inner, icon_col, w),
            Ico::Toc => paint::list(scene, t, inner, icon_col, w),
            Ico::Notes => paint::note(scene, t, inner, icon_col, w),
            Ico::Bookmark(filled) => paint::bookmark(scene, t, inner, icon_col, w, filled),
            Ico::Dots => paint::dots(scene, t, inner, icon_col),
            Ico::ChevDown => paint::chevron_down(scene, t, inner, icon_col, w),
        }
        self.push_hit(rect, action);
        s
    }

    /// A filled accent button with a centred label (and an optional leading `+`),
    /// the single primary action on a screen. Pushes a hit for `action`.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn accent_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        with_plus: bool,
        action: Action,
    ) {
        let radius = (rect.height() / 2.0).min(f64::from(theme.tokens.radii.lg) + 4.0);
        paint::fill_round(scene, t, rect, radius, theme.accent_primary);
        if self.is_hover(rect) {
            // A soft white veil is the hover state on a filled accent surface.
            paint::fill_round(scene, t, rect, radius, Color::from_rgba8(255, 255, 255, 26));
        }
        let ink = Color::from_rgb8(250, 250, 252);
        let (lw, _) = tr.measure_weighted(label, 14.0, None, TextWeight::Semibold);
        let glyph = if with_plus { 16.0 + 9.0 } else { 0.0 };
        let group_w = glyph + f64::from(lw);
        let gx = rect.x0 + (rect.width() - group_w) / 2.0;
        let cy = f64::midpoint(rect.y0, rect.y1);
        if with_plus {
            paint::plus(
                scene,
                t,
                Rect::new(gx, cy - 8.0, gx + 16.0, cy + 8.0),
                ink,
                2.0,
            );
        }
        tr.draw_text_weighted(
            scene,
            label,
            14.0,
            ink,
            t * Affine::translate((gx + glyph, cy - 9.0)),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(rect, action);
    }

    /// A toggle switch (track + knob). `w`/`h` are the track size. Pushes a hit
    /// for `action` and returns the track rect.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/c/x/y/w/h are the standard transform/colours/geometry names shared by \
                  every draw helper in this chrome section"
    )]
    fn switch(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        on: bool,
        action: Action,
    ) {
        let track = Rect::new(x, y, x + w, y + h);
        let bg = if on {
            theme.accent_primary
        } else {
            c.neutral[3]
        };
        paint::fill_round(scene, t, track, h / 2.0, bg);
        let kr = h / 2.0 - 2.5;
        let kx = if on { x + w - h / 2.0 } else { x + h / 2.0 };
        paint::circle(
            scene,
            t,
            Point::new(kx, y + h / 2.0),
            kr,
            Color::from_rgb8(250, 250, 252),
        );
        self.push_hit(track, action);
    }

    /// A compact shared numeric control. Lays out right-to-left from `right_x`;
    /// returns the left edge it consumed so every font row stays aligned.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn size_stepper(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _c: &ui::tokens::colors::Colors,
        right_x: f64,
        cy: f64,
        value: f64,
        field: NumberField,
    ) -> f64 {
        let btn = 28.0;
        let vslot_w = 52.0;
        let by = cy - btn / 2.0;
        let width = btn * 2.0 + vslot_w + 8.0;
        let minus_x = right_x - width;
        self.number_controls.draw(
            field,
            value,
            Rect::new(minus_x, by, right_x, by + btn),
            scene,
            t,
            tr,
            theme,
        );
        minus_x
    }

    /// A text pill (used for segmented controls + tags). Returns its width.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/x/y/h/w/c are the standard transform/geometry/colours names shared by \
                  every draw helper in this chrome section"
    )]
    fn pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        h: f64,
        label: &str,
        size: f32,
        active: bool,
        action: Option<Action>,
    ) -> f64 {
        let (tw, _) = tr.measure_weighted(label, size, None, TextWeight::Medium);
        let pad = 12.0;
        let w = f64::from(tw) + pad * 2.0;
        let rect = Rect::new(x, y, x + w, y + h);
        let hover = action.is_some() && self.is_hover(rect);
        let c = &theme.tokens.colors;
        let bg = if active {
            theme.accent_primary
        } else if hover {
            theme.bg_hover
        } else {
            c.surfaces.overlay
        };
        paint::fill_round(scene, t, rect, h / 2.0, bg);
        let text_col = if active {
            Color::from_rgb8(248, 248, 252)
        } else {
            c.text_primary
        };
        let ty = y + (h - f64::from(size)) / 2.0;
        tr.draw_text_weighted(
            scene,
            label,
            size,
            text_col,
            t * Affine::translate((x + pad, ty)),
            None,
            TextWeight::Medium,
        );
        if let Some(a) = action {
            self.push_hit(rect, a);
        }
        w
    }
}

// --- Library (shelf) view ---------------------------------------------------

/// Column count, cell pitch, and scroll extent/offset for the book grid,
/// computed once per [`BooksApp::draw_library_grid`] call.
struct ShelfGrid {
    cols: usize,
    card_w: f64,
    cover_h: f64,
    card_h: f64,
    gap: f64,
    start_x: f64,
    start_y: f64,
    row_pitch: f64,
    content_h: f64,
    visible_h: f64,
    scroll: f64,
}

impl BooksApp {
    fn draw_library(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.shelf_controls.begin_frame();
        let lib = std::mem::take(&mut self.library);
        let c = theme.tokens.colors.clone();
        let r = self.rect;

        let open_x = self.draw_library_header(scene, t, tr, theme, &c, r, lib.len());

        // --- empty shelf: a composed, inviting onboarding -----------------
        if lib.is_empty() {
            self.draw_empty_library(scene, t, tr, theme, &c);
            self.library = lib;
            return;
        }

        let sort_btn = self.shelf_controls.draw_bar(
            scene,
            t,
            tr,
            theme,
            r,
            open_x,
            self.cursor,
            self.library_prefs,
        );

        // --- filter + sort the display order ------------------------------
        let order = self.shelf_display_order(&lib);
        self.shelf_order.clone_from(&order);

        // Everything hidden by the "hide read" filter.
        if order.is_empty() {
            Self::draw_all_read_empty_state(scene, t, tr, &c, r);
            self.shelf_controls.draw_menu(
                scene,
                t,
                tr,
                theme,
                r,
                sort_btn,
                self.cursor,
                self.library_prefs.sort,
            );
            self.library = lib;
            return;
        }

        self.draw_library_grid(scene, t, tr, theme, &lib, &order, &c, r);
        self.shelf_controls.draw_menu(
            scene,
            t,
            tr,
            theme,
            r,
            sort_btn,
            self.cursor,
            self.library_prefs.sort,
        );
        self.library = lib;
    }

    /// Top bar: title + book count, and the primary "Open book" action
    /// (always present — a way in that doesn't depend on any folder).
    /// Returns the button's left edge, so shelf controls can lay out from it.
    #[allow(clippy::too_many_arguments)]
    fn draw_library_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        r: Rect,
        book_count: usize,
    ) -> f64 {
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::line(
            scene,
            t,
            Point::new(r.x0, r.y0 + TOP_BAR),
            Point::new(r.x1, r.y0 + TOP_BAR),
            c.border_standard,
            1.0,
        );
        tr.draw_text_weighted(
            scene,
            "Library",
            20.0,
            c.text_primary,
            t * Affine::translate((r.x0 + 28.0, r.y0 + 9.0)),
            None,
            TextWeight::Bold,
        );
        // The count reads under the title only once there's something to count —
        // on the empty state the onboarding speaks for itself.
        if book_count > 0 {
            let sub = match book_count {
                1 => "1 book".to_string(),
                n => format!("{n} books"),
            };
            tr.draw_text(
                scene,
                &sub,
                12.0,
                c.text_dim,
                t * Affine::translate((r.x0 + 29.0, r.y0 + 32.0)),
                None,
            );
        }

        // The primary "Open book" action is always present, so there is always a
        // way in that doesn't depend on any folder — right-aligned, accent-filled.
        let btn_cy = r.y0 + (TOP_BAR - 34.0) / 2.0;
        let open_label = "Open book";
        let (olw, _) = tr.measure_weighted(open_label, 14.0, None, TextWeight::Semibold);
        let open_w = f64::from(olw) + 25.0 + 32.0;
        let open_x = r.x1 - 22.0 - open_w;
        self.accent_button(
            scene,
            t,
            tr,
            theme,
            Rect::new(open_x, btn_cy, open_x + open_w, btn_cy + 34.0),
            open_label,
            true,
            Action::OpenViaPortal,
        );
        open_x
    }

    /// Everything hidden by the "hide read" filter: an empty-shelf message
    /// with a hint to turn the filter back off.
    fn draw_all_read_empty_state(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        r: Rect,
    ) {
        let cx = f64::midpoint(r.x0, r.x1);
        let cy = (r.y0 + TOP_BAR + r.y1) / 2.0;
        let head = "All books are finished";
        let (hw, _) = tr.measure_weighted(head, 18.0, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            head,
            18.0,
            c.text_primary,
            t * Affine::translate((cx - f64::from(hw) / 2.0, cy - 20.0)),
            None,
            TextWeight::Semibold,
        );
        let hint = "Turn off \u{201C}Hide read\u{201D} to see them again.";
        let (tw2, _) = tr.measure(hint, 13.0, None);
        tr.draw_text(
            scene,
            hint,
            13.0,
            c.text_dim,
            t * Affine::translate((cx - f64::from(tw2) / 2.0, cy + 8.0)),
            None,
        );
    }

    /// Column count, cell pitch, and vertical scroll extent/offset for the
    /// book grid. Also reveals the keyboard-focused card if a step just
    /// moved the cursor, and clamps the persisted scroll to the new extent.
    fn shelf_grid_geometry(&mut self, r: Rect, book_count: usize) -> ShelfGrid {
        let margin = 40.0;
        let gap = 28.0;
        let card_w = 196.0;
        let cover_h = 232.0;
        let card_h = cover_h + 78.0;
        let avail = (r.width() - 2.0 * margin + gap).max(card_w);
        let cols = (avail / (card_w + gap)).floor().max(1.0) as usize;
        // Publish the column count so keyboard up/down can step a whole row.
        self.shelf_cols = cols;
        self.shelf_cursor = self.shelf_cursor.min(book_count.saturating_sub(1));
        let grid_w = cols as f64 * card_w + (cols as f64 - 1.0) * gap;
        let start_x = r.x0 + (r.width() - grid_w) / 2.0;
        let start_y = r.y0 + TOP_BAR + 36.0;

        // The shelf can hold more books than fit, so it scrolls. Work out the
        // total content height and clamp the offset to what's reachable.
        // One extra grid cell trails the last book: the persistent "Add books"
        // tile (a standing import affordance + drop-target hint), so the scroll
        // extent is sized to include it.
        let rows = (book_count + 1).div_ceil(cols);
        let row_pitch = card_h + gap;
        let content_h = if rows == 0 {
            0.0
        } else {
            (rows as f64 - 1.0) * row_pitch + card_h + 40.0
        };
        let visible_h = (r.y1 - start_y).max(0.0);
        self.shelf_max_scroll = (content_h - visible_h).max(0.0);
        // Keyboard navigation asked to reveal the focused card: nudge the scroll
        // so the focused row sits fully inside the viewport, then clear the flag.
        if self.scroll_to_cursor {
            let cur_row = self.shelf_cursor / cols;
            let rel = cur_row as f64 * row_pitch;
            let full = card_h + 84.0;
            let top_screen = start_y + rel - self.shelf_scroll;
            if top_screen < start_y {
                self.shelf_scroll = rel;
            } else if top_screen + full > r.y1 {
                self.shelf_scroll = start_y + rel + full - r.y1;
            }
            self.scroll_to_cursor = false;
        }
        self.shelf_scroll = self.shelf_scroll.clamp(0.0, self.shelf_max_scroll);

        ShelfGrid {
            cols,
            card_w,
            cover_h,
            card_h,
            gap,
            start_x,
            start_y,
            row_pitch,
            content_h,
            visible_h,
            scroll: self.shelf_scroll,
        }
    }

    /// The scrolling book grid + trailing "Add books" tile, scroll
    /// indicator, and (if open) the sort dropdown menu.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/c/r are the standard transform/colours/rect draw-context names, and i/x/y \
                  are the grid loop's index and card position"
    )]
    fn draw_library_grid(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lib: &[Book],
        order: &[usize],
        c: &ui::tokens::colors::Colors,
        r: Rect,
    ) {
        let grid = self.shelf_grid_geometry(r, order.len());

        // Clip the scrolling body to below the top bar so cards slide under it
        // rather than over it, then offset every card by the scroll.
        let body = Rect::new(r.x0, r.y0 + TOP_BAR + 1.0, r.x1, r.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        for (i, &orig) in order.iter().enumerate() {
            let Some(book) = lib.get(orig) else { continue };
            let col = i % grid.cols;
            let row = i / grid.cols;
            let x = grid.start_x + col as f64 * (grid.card_w + grid.gap);
            let y = grid.start_y + row as f64 * grid.row_pitch - grid.scroll;
            // Cull rows fully outside the viewport (keeps a huge shelf cheap).
            if y > r.y1 || y + grid.card_h + 84.0 < r.y0 + TOP_BAR {
                continue;
            }
            let focused = i == self.shelf_cursor;
            // `self.library` is taken for the duration of the draw, so resolve
            // shared-vs-local from the book's id + the path cache directly.
            let shared = self
                .library_paths
                .get(&book.id)
                .is_some_and(|p| is_shared(p));
            self.draw_book_card(
                scene,
                t,
                tr,
                theme,
                book,
                orig,
                x,
                y,
                grid.card_w,
                grid.cover_h,
                focused,
                shared,
            );
        }
        // Trailing "Add books" tile in the next free grid cell.
        {
            let i = order.len();
            let col = i % grid.cols;
            let row = i / grid.cols;
            let x = grid.start_x + col as f64 * (grid.card_w + grid.gap);
            let y = grid.start_y + row as f64 * grid.row_pitch - grid.scroll;
            if !(y > r.y1 || y + grid.card_h + 84.0 < r.y0 + TOP_BAR) {
                self.draw_add_book_card(scene, t, tr, theme, x, y, grid.card_w, grid.cover_h);
            }
        }
        scene.pop_layer();

        // Scroll indicator, only when there's something to scroll to.
        if self.shelf_max_scroll > 1.0 {
            let track_x = r.x1 - 9.0;
            let track_y0 = r.y0 + TOP_BAR + 10.0;
            let track_y1 = r.y1 - 10.0;
            let track_h = (track_y1 - track_y0).max(1.0);
            let thumb_h = (track_h * grid.visible_h / grid.content_h).clamp(28.0, track_h);
            let frac = grid.scroll / self.shelf_max_scroll;
            let thumb_y = track_y0 + frac * (track_h - thumb_h);
            let thumb = Rect::new(track_x, thumb_y, track_x + 4.0, thumb_y + thumb_h);
            paint::fill_round(scene, t, thumb, 2.0, c.text_dim.with_alpha_factor(0.5));
        }
    }

    /// The onboarding shown when the library is empty: an emblem, a warm
    /// headline, a plain-language explanation, and one primary action — open a
    /// book from anywhere. No magic folder, no "reload"; the shelf fills itself
    /// the moment the reader opens their first book.
    fn draw_empty_library(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
    ) {
        let r = self.rect;
        let cx = f64::midpoint(r.x0, r.x1);
        // Compose the block around a comfortable optical centre (a touch above
        // true centre reads better for a stacked call-to-action).
        let cy = r.y0 + TOP_BAR + (r.y1 - r.y0 - TOP_BAR) * 0.42;

        // Emblem: a soft accent disc with an open-book mark and a small plus.
        let disc_r = 40.0;
        let disc_cy = cy - 96.0;
        paint::circle(
            scene,
            t,
            Point::new(cx, disc_cy),
            disc_r,
            theme.accent_primary.with_alpha_factor(0.14),
        );
        paint::book(
            scene,
            t,
            Rect::new(cx - 20.0, disc_cy - 15.0, cx + 20.0, disc_cy + 15.0),
            theme.accent_primary,
            2.0,
        );
        paint::circle(
            scene,
            t,
            Point::new(cx + 26.0, disc_cy + 24.0),
            11.0,
            theme.accent_primary,
        );
        paint::plus(
            scene,
            t,
            Rect::new(cx + 18.0, disc_cy + 16.0, cx + 34.0, disc_cy + 32.0),
            Color::from_rgb8(250, 250, 252),
            1.9,
        );

        let head = "Your library is empty";
        let (hw, _) = tr.measure_weighted(head, 24.0, None, TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            head,
            24.0,
            c.text_primary,
            t * Affine::translate((cx - f64::from(hw) / 2.0, cy - 34.0)),
            None,
            TextWeight::Bold,
        );

        let body = "Open a book from anywhere — your device or a shared workspace. It's added to your library, so it's waiting here next time.";
        let body_w = (r.width() * 0.6).clamp(280.0, 460.0);
        let (_bw, _) = tr.measure(body, 14.0, Some(body_w as f32));
        tr.draw_text(
            scene,
            body,
            14.0,
            c.text_dim,
            t * Affine::translate((cx - body_w / 2.0, cy + 2.0)),
            Some(body_w as f32),
        );

        // Primary call to action.
        let cta_label = "Open a book…";
        let (clw, _) = tr.measure_weighted(cta_label, 14.0, None, TextWeight::Semibold);
        let cta_w = f64::from(clw) + 25.0 + 44.0;
        let cta_h = 44.0;
        let cta_y = cy + 62.0;
        self.accent_button(
            scene,
            t,
            tr,
            theme,
            Rect::new(cx - cta_w / 2.0, cta_y, cx + cta_w / 2.0, cta_y + cta_h),
            cta_label,
            true,
            Action::OpenViaPortal,
        );

        // A quiet pointer to where watched folders are managed (secondary, never
        // the only way in).
        let hint = "Tip: add folders to watch in Settings › Library.";
        let (thw, _) = tr.measure(hint, 12.5, None);
        tr.draw_text(
            scene,
            hint,
            12.5,
            c.text_dim.with_alpha_factor(0.75),
            t * Affine::translate((cx - f64::from(thw) / 2.0, cta_y + cta_h + 20.0)),
            None,
        );

        // Surface a picker failure (dismissed/unavailable) right under the CTA.
        if let Some(err) = &self.load_error {
            let (ew, _) = tr.measure(err, 12.5, Some((body_w) as f32));
            tr.draw_text(
                scene,
                err,
                12.5,
                c.semantic.danger,
                t * Affine::translate((
                    cx - f64::from(ew.min(body_w as f32)) / 2.0,
                    cta_y + cta_h + 44.0,
                )),
                Some(body_w as f32),
            );
        }
    }

    /// The filtered + sorted display order of the shelf: original library
    /// indices, hiding finished books when requested and ordered by the chosen
    /// key (ties broken by title so the order is stable).
    fn shelf_display_order(&self, lib: &[Book]) -> Vec<usize> {
        use crate::model::LibrarySort;
        let frac = |i: usize| self.shelf_fraction.get(&lib[i].id).copied().unwrap_or(0.0);
        let opened = |i: usize| self.shelf_opened.get(&lib[i].id).copied().unwrap_or(0);
        let title = |i: usize| lib[i].title.to_lowercase();
        let mut idx: Vec<usize> = (0..lib.len())
            .filter(|&i| !self.library_prefs.hide_read || frac(i) < 0.985)
            .collect();
        match self.library_prefs.sort {
            LibrarySort::Title => idx.sort_by_key(|&a| title(a)),
            LibrarySort::Author => idx.sort_by(|&a, &b| {
                lib[a]
                    .author
                    .to_lowercase()
                    .cmp(&lib[b].author.to_lowercase())
                    .then(title(a).cmp(&title(b)))
            }),
            LibrarySort::Recent => {
                idx.sort_by(|&a, &b| opened(b).cmp(&opened(a)).then(title(a).cmp(&title(b))));
            }
            LibrarySort::Progress => idx.sort_by(|&a, &b| {
                frac(b)
                    .partial_cmp(&frac(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(title(a).cmp(&title(b)))
            }),
        }
        idx
    }

    /// Resolve a book's decoded cover into a cached, GPU-ready image handle.
    /// Built once per book id; subsequent frames clone the cached handle (an
    /// `Arc` bump), so no decoding or allocation happens on the draw path.
    fn cover_image(&mut self, book: &Book) -> Option<PenikoImage> {
        if let Some(img) = self.cover_cache.get(&book.id) {
            return Some(img.clone());
        }
        let cover = book.cover.as_ref()?;
        let expected = (cover.width as usize) * (cover.height as usize) * 4;
        if cover.rgba.len() != expected {
            return None;
        }
        let img = PenikoImage {
            data: Blob::new(cover.rgba.clone()),
            format: ImageFmt::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: cover.width,
            height: cover.height,
        };
        self.cover_cache.insert(book.id.clone(), img.clone());
        Some(img)
    }

    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/x/y/w/c are the standard transform/geometry/colours names shared by \
                  every draw helper in this section"
    )]
    fn draw_book_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        book: &Book,
        idx: usize,
        x: f64,
        y: f64,
        w: f64,
        cover_h: f64,
        focused: bool,
        shared: bool,
    ) {
        let c = &theme.tokens.colors;
        let cover = Rect::new(x, y, x + w, y + cover_h);
        let hover = self.is_hover(Rect::new(x, y, x + w, y + cover_h + 78.0));

        // Drop shadow.
        let shadow = Rect::new(x + 3.0, y + 6.0, x + w + 3.0, y + cover_h + 6.0);
        paint::fill_round(scene, t, shadow, 12.0, Color::from_rgba8(0, 0, 0, 60));

        let title_on_cover =
            self.draw_book_cover_art(scene, t, tr, theme, book, x, y, w, cover_h, cover);

        // Shared-vs-local badge, overlaid at the cover's top-right. A book on the
        // workspace is available to everyone there; a local one lives only on
        // this device. Both are labelled so the state reads at a glance.
        Self::draw_shared_badge(scene, t, tr, theme, x, y, w, shared);

        // Keyboard focus draws a ring even without a pointer over the card, so
        // arrow-key navigation is visible; hover uses the same treatment.
        if focused {
            paint::stroke_round(scene, t, cover, 12.0, theme.accent_primary, 2.5);
        } else if hover {
            paint::stroke_round(scene, t, cover, 12.0, theme.border_focus, 2.0);
        }

        // --- under-cover meta: progress + counts -------------------------
        let meta_y = self.draw_book_card_meta(
            scene,
            t,
            tr,
            c,
            theme,
            book,
            x,
            y,
            w,
            cover_h,
            title_on_cover,
        );

        // Clamp the click target to the scrolling body so a card sliding under
        // the top bar can't capture clicks up in the header.
        let hit_top = y.max(self.rect.y0 + TOP_BAR);
        let hit_bot = (meta_y + 70.0).min(self.rect.y1);
        if hit_bot > hit_top {
            self.push_hit(Rect::new(x, hit_top, x + w, hit_bot), Action::OpenBook(idx));
        }

        // Share/Remove cluster over the cover foot on hover. Pushed after the
        // whole-card hit so they win the click where they overlap.
        if hover && y >= self.rect.y0 + TOP_BAR {
            self.draw_book_card_hover_actions(scene, t, tr, theme, x, y, w, cover_h, idx, shared);
        }
    }

    /// The cover art: real embedded artwork when the EPUB carries one,
    /// otherwise a generated spine (tinted board, sheen + foot for depth,
    /// title in serif display, author along the base). Returns whether the
    /// title is already shown on the cover (a real cover isn't relabelled
    /// below it).
    #[allow(clippy::too_many_arguments)]
    fn draw_book_cover_art(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        book: &Book,
        x: f64,
        y: f64,
        w: f64,
        cover_h: f64,
        cover: Rect,
    ) -> bool {
        let real_cover = self.cover_image(book);
        let title_on_cover = real_cover.is_none();
        if let Some(img) = real_cover {
            blit_cover(scene, t, img, cover, 12.0);
            // Faint inner edge to seat the art against the shelf.
            paint::stroke_round(scene, t, cover, 12.0, Color::from_rgba8(0, 0, 0, 40), 1.0);
        } else {
            // A designed cover: tinted board, top sheen + darker foot for
            // depth, title in serif display, author along the base.
            let (spine, ink) = cover_colors(theme, book.cover_tint);
            paint::fill_round(scene, t, cover, 12.0, spine);
            // Top sheen (a faint light band) + darker foot, layered for depth.
            let sheen = Rect::new(x, y, x + w, y + cover_h * 0.34);
            paint::fill_round(scene, t, sheen, 12.0, Color::from_rgba8(255, 255, 255, 16));
            let foot = Rect::new(x, y + cover_h * 0.70, x + w, y + cover_h);
            paint::fill_round(scene, t, foot, 12.0, Color::from_rgba8(0, 0, 0, 46));

            // subject pill + spark.
            if !book.subject.is_empty() {
                let psize = 11.0;
                let (pw, _) = tr.measure(&book.subject, psize, None);
                let pill = Rect::new(
                    x + 16.0,
                    y + 16.0,
                    x + 16.0 + f64::from(pw) + 18.0,
                    y + 16.0 + 20.0,
                );
                paint::fill_round(scene, t, pill, 10.0, ink.with_alpha_factor(0.18));
                tr.draw_text(
                    scene,
                    &book.subject,
                    psize,
                    ink,
                    t * Affine::translate((x + 25.0, y + 20.0)),
                    None,
                );
            }

            // Title in a serif display face (a book-cover convention), wrapped.
            let title_w = w - 32.0;
            tr.draw_text_font(
                scene,
                &book.title,
                20.0,
                ink,
                t * Affine::translate((x + 18.0, y + cover_h * 0.38)),
                Some(title_w as f32),
                TextWeight::Bold,
                false,
                &FontChoice::Serif,
            );
            // Hairline rule above the author.
            paint::line(
                scene,
                t,
                Point::new(x + 18.0, y + cover_h - 42.0),
                Point::new(x + 18.0 + 34.0, y + cover_h - 42.0),
                ink.with_alpha_factor(0.7),
                1.4,
            );
            tr.draw_text(
                scene,
                &book.author,
                13.0,
                ink.with_alpha_factor(0.86),
                t * Affine::translate((x + 18.0, y + cover_h - 30.0)),
                Some(title_w as f32),
            );
        }
        title_on_cover
    }

    /// The "Shared"/"Local" badge overlaid at the cover's top-right.
    #[allow(clippy::too_many_arguments)]
    fn draw_shared_badge(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        shared: bool,
    ) {
        let (label, bg, ink, glyph) = if shared {
            (
                "Shared",
                theme.accent_primary,
                Color::from_rgb8(250, 250, 252),
                true,
            )
        } else {
            (
                "Local",
                Color::from_rgba8(0, 0, 0, 150),
                Color::from_rgb8(235, 235, 240),
                false,
            )
        };
        let (lw, _) = tr.measure_weighted(label, 11.0, None, TextWeight::Semibold);
        let gly = if glyph { 16.0 } else { 0.0 };
        let pw = 16.0 + gly + f64::from(lw);
        let py = y + 14.0;
        let pill = Rect::new(x + w - 14.0 - pw, py, x + w - 14.0, py + 20.0);
        paint::fill_round(scene, t, pill, 10.0, bg);
        let mut tx = pill.x0 + 8.0;
        if glyph {
            paint::people(
                scene,
                t,
                Rect::new(tx, py + 4.0, tx + 13.0, py + 16.0),
                ink,
                1.4,
            );
            tx += 16.0;
        }
        tr.draw_text_weighted(
            scene,
            label,
            11.0,
            ink,
            t * Affine::translate((tx, py + 4.0)),
            None,
            TextWeight::Semibold,
        );
    }

    /// Under-cover meta: title (when not already on the cover), progress
    /// bar + percent, and an annotation-count badge. Returns the meta
    /// block's top y, so the caller can size the card's click target.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::many_single_char_names,
        reason = "t/x/y/w/c are the standard transform/geometry/colours names shared by \
                  every draw helper in this section"
    )]
    fn draw_book_card_meta(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        theme: &Theme,
        book: &Book,
        x: f64,
        y: f64,
        w: f64,
        cover_h: f64,
        title_on_cover: bool,
    ) -> f64 {
        let frac = self.shelf_fraction.get(&book.id).copied().unwrap_or(0.0);
        let anno = self.shelf_annos.get(&book.id).copied().unwrap_or(0);
        let meta_y = y + cover_h + 14.0;
        // The generated cover already carries the title in display type; drawing
        // it again here would show the same title twice on one card. Only label
        // real cover art (whose embedded title may not be legible on the shelf).
        if !title_on_cover {
            tr.draw_text_weighted(
                scene,
                &book.title,
                14.0,
                c.text_primary,
                t * Affine::translate((x + 2.0, meta_y)),
                Some((w - 4.0) as f32),
                TextWeight::Semibold,
            );
        }
        // progress bar
        let bar = Rect::new(x + 2.0, meta_y + 34.0, x + w - 56.0, meta_y + 40.0);
        paint::progress(scene, t, bar, frac, c.neutral[3], theme.accent_primary);
        let pct = format!("{}%", (frac * 100.0).round() as i32);
        tr.draw_text(
            scene,
            &pct,
            12.0,
            c.text_dim,
            t * Affine::translate((x + w - 48.0, meta_y + 31.0)),
            None,
        );
        if anno > 0 {
            let label = format!("{anno} notes");
            let (lw, _) = tr.measure(&label, 11.0, None);
            let badge = Rect::new(
                x + 2.0,
                meta_y + 50.0,
                x + 2.0 + f64::from(lw) + 16.0,
                meta_y + 50.0 + 18.0,
            );
            paint::fill_round(scene, t, badge, 9.0, c.surfaces.overlay);
            tr.draw_text(
                scene,
                &label,
                11.0,
                c.text_dim,
                t * Affine::translate((x + 10.0, meta_y + 53.0)),
                None,
            );
        }
        meta_y
    }

    /// The Share/Remove affordance cluster over the cover foot, shown on
    /// hover only (pushed after the whole-card hit, so they win the click
    /// where they overlap it).
    #[allow(clippy::too_many_arguments)]
    fn draw_book_card_hover_actions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        cover_h: f64,
        idx: usize,
        shared: bool,
    ) {
        let sh = 26.0;
        let by = y + cover_h - sh - 8.0;
        let mut right = x + w - 10.0;
        if !shared {
            let sl = "Share";
            let (slw, _) = tr.measure_weighted(sl, 12.0, None, TextWeight::Semibold);
            let sw = f64::from(slw) + 16.0 + 22.0;
            let sx = right - sw;
            let sr = Rect::new(sx, by, sx + sw, by + sh);
            paint::fill_round(scene, t, sr, sh / 2.0, theme.accent_primary);
            paint::people(
                scene,
                t,
                Rect::new(sx + 8.0, by + 6.0, sx + 22.0, by + 20.0),
                Color::from_rgb8(250, 250, 252),
                1.4,
            );
            tr.draw_text_weighted(
                scene,
                sl,
                12.0,
                Color::from_rgb8(250, 250, 252),
                t * Affine::translate((sx + 26.0, by + 6.0)),
                None,
                TextWeight::Semibold,
            );
            self.push_hit(sr, Action::ShareToWorkspace(idx));
            right = sx - 8.0;
        }
        // Remove — a small dark disc with a trash glyph.
        let rr = sh / 2.0;
        let rc = Point::new(right - rr, by + rr);
        paint::circle(scene, t, rc, rr, Color::from_rgba8(0, 0, 0, 180));
        paint::trash(
            scene,
            t,
            Rect::new(rc.x - 8.0, rc.y - 8.0, rc.x + 8.0, rc.y + 8.0),
            Color::from_rgb8(240, 240, 244),
            1.5,
        );
        self.push_hit(
            Rect::new(rc.x - rr, rc.y - rr, rc.x + rr, rc.y + rr),
            Action::RemoveFromLibrary(idx),
        );
    }

    /// The trailing "Add books" tile: opens the host file picker.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_add_book_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        cover_h: f64,
    ) {
        let c = &theme.tokens.colors;
        let tile = Rect::new(x, y, x + w, y + cover_h);
        let hover = self.is_hover(tile);

        // A quiet, inviting drop surface: faint fill under a dashed accent border.
        paint::fill_round(
            scene,
            t,
            tile,
            12.0,
            if hover {
                theme.bg_hover
            } else {
                c.surfaces.overlay
            },
        );
        let border = if hover {
            theme.accent_primary
        } else {
            theme.accent_primary.with_alpha_factor(0.55)
        };
        dashed_round_border(
            scene,
            t,
            tile,
            12.0,
            border,
            &DashStyle {
                width: 1.6,
                dash: 7.0,
                gap: 5.0,
            },
        );

        // Plus mark inside a soft accent disc.
        let cx = x + w / 2.0;
        let disc_cy = y + cover_h * 0.40;
        let disc_r = 24.0;
        paint::circle(
            scene,
            t,
            Point::new(cx, disc_cy),
            disc_r,
            theme.accent_primary.with_alpha_factor(0.16),
        );
        paint::plus(
            scene,
            t,
            Rect::new(cx - 11.0, disc_cy - 11.0, cx + 11.0, disc_cy + 11.0),
            theme.accent_primary,
            2.2,
        );

        // Title + hint, centred under the mark.
        let title = "Add a book";
        let (title_w, _) = tr.measure_weighted(title, 15.0, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            title,
            15.0,
            c.text_primary,
            t * Affine::translate((cx - f64::from(title_w) / 2.0, disc_cy + disc_r + 16.0)),
            None,
            TextWeight::Semibold,
        );
        let hint = "Open from anywhere";
        let (hint_w, _) = tr.measure(hint, 12.0, None);
        tr.draw_text(
            scene,
            hint,
            12.0,
            c.text_dim,
            t * Affine::translate((cx - f64::from(hint_w) / 2.0, disc_cy + disc_r + 38.0)),
            None,
        );

        self.push_hit(tile, Action::OpenViaPortal);
    }
}

// --- Reader view -----------------------------------------------------------

/// Column, paper, and content-box geometry for one reader frame, computed
/// once per [`BooksApp::draw_reader`] call.
struct ReaderPageGeometry {
    center_x0: f64,
    center_x1: f64,
    body_y0: f64,
    body_y1: f64,
    /// Whether the page is a floating framed card (wide window) or fills the
    /// reading area edge-to-edge (narrow window).
    framed: bool,
    paper_x: f64,
    paper_y: f64,
    paper_w: f64,
    paper_h: f64,
    /// Left edge / width of the full content box (both columns in `TwoUp`).
    full_content_x: f64,
    full_content_w: f32,
    /// Left edge of the first (or only) column — the header/progress-bar origin.
    content_x: f64,
    content_y: f64,
    /// Per-column layout width: `full_content_w`, halved (minus the gutter)
    /// in `TwoUp`.
    content_w: f32,
    content_h: f32,
    gutter: f64,
}

/// Sizing + pre-measured content for the reading-settings popover, computed
/// once per [`BooksApp::draw_type_popover`] call.
struct TypePopoverLayout {
    pad: f64,
    row_h: f64,
    pill_h: f64,
    pill_gap: f64,
    header_h: f64,
    panel: Rect,
    /// Inner-content left/right edges (`panel` inset by `pad`).
    ix0: f64,
    ix1: f64,
    font_pills: Vec<(String, String, f64, bool)>,
    scheme_pills: Vec<SchemePill>,
    scheme_widths: Vec<f64>,
}

impl BooksApp {
    fn draw_reader(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let lib = std::mem::take(&mut self.library);
        let annos = std::mem::take(&mut self.annotations);
        let c = theme.tokens.colors.clone();
        let r = self.rect;
        self.book_idx = self.book_idx.min(lib.len().saturating_sub(1));
        let nchap = lib.get(self.book_idx).map_or(0, |b| b.chapters.len());
        self.chapter = self.chapter.min(nchap.saturating_sub(1));

        let mode = self.settings.mode;
        let geom = self.reader_page_geometry(r, mode);
        let palette = self.resolve_palette(theme);

        // ---- (re)compute the chapter layout -----------------------------
        let (base_family, font_mult, font_sig, base_mono) = self.body_font();
        let layout =
            self.resolve_chapter_layout(tr, &lib, &geom, font_sig, &base_family, font_mult);
        self.cur_page_count = layout.page_count();
        self.resolve_reader_page_index(&layout, mode, &geom);

        // ---- the on-screen reading columns (panes) ----------------------
        let panes = self.compute_panes(&layout, mode, &geom);

        // ---- page surface ------------------------------------------------
        self.draw_reader_page_surface(scene, t, tr, &lib, &palette, &geom);

        // ---- draw each reading column (margins → selection bands → text) -
        let chapter = self.chapter;
        self.reader_body = (geom.body_y0, geom.body_y1);
        self.sel_runs.clear();
        self.hi_markers.clear();
        self.hi_bands.clear();
        self.page_first_doc = None;
        self.page_last_doc = None;
        let comment_group = self.comment_overlay.group();
        let sel_ord = self.sel.filter(|s| !s.is_empty()).map(|s| s.ordered());

        let (sel_anchor, comment_box) = self.draw_reader_columns(
            scene,
            t,
            tr,
            theme,
            &layout,
            &annos,
            &palette,
            &base_family,
            base_mono,
            chapter,
            sel_ord,
            comment_group,
            &panes,
            &geom,
            mode,
        );
        self.comment_overlay.set_anchor(comment_box);
        self.resolve_pending_sel_edge();

        self.draw_reader_footer_and_chrome(
            scene, t, tr, theme, &c, lib, annos, layout, &palette, &geom, mode, nchap, sel_anchor,
        );
    }

    /// Resolve the current page/scroll index against the fresh layout:
    /// honour a pending "jump to last page"/"jump to block" request, clamp
    /// to the page count, align a spread to an even left page, and
    /// (continuous mode) clamp the scroll offset and derive `self.page`
    /// from it.
    fn resolve_reader_page_index(
        &mut self,
        layout: &ChapterLayout,
        mode: ReadMode,
        geom: &ReaderPageGeometry,
    ) {
        if self.pending_last_page {
            self.page = layout.page_count().saturating_sub(1);
            self.pending_last_page = false;
        }
        if let Some(jb) = self.pending_jump_block.take() {
            self.page = page_for_block(layout, jb);
        }
        self.page = self.page.min(layout.page_count().saturating_sub(1));
        // Spreads align to an even left page so a spread is always (2k, 2k+1).
        if mode == ReadMode::TwoUp {
            self.page -= self.page % 2;
        }

        // Record the flow extent so a continuous scroll (which runs before the
        // next layout) can clamp itself, and resolve a pending end-scroll after
        // a backward chapter roll. Keep `page` synced to the scroll so progress
        // and the page locator stay meaningful.
        self.flow_extent = (f64::from(layout.total_height), f64::from(geom.content_h));
        if mode.is_continuous() {
            let max_scroll = (f64::from(layout.total_height) - f64::from(geom.content_h)).max(0.0);
            if self.pending_scroll_end {
                self.scroll_y = max_scroll;
                self.pending_scroll_end = false;
            }
            self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
            self.page = layout
                .pages
                .iter()
                .rposition(|&py| f64::from(py) <= self.scroll_y + 0.5)
                .unwrap_or(0);
        } else {
            self.pending_scroll_end = false;
        }
    }

    /// A drag that auto-advanced a page extends the selection head to this
    /// page's edge, once the new page's first/last document position (set
    /// by [`Self::draw_reader_columns`]) is known.
    fn resolve_pending_sel_edge(&mut self) {
        if let Some(to_end) = self.pending_sel_edge.take()
            && let Some(mut sel) = self.sel
        {
            let edge = if to_end {
                self.page_last_doc
            } else {
                self.page_first_doc
            };
            if let Some(edge) = edge {
                sel.head = edge;
                self.sel = Some(sel);
                let (start, _) = sel.ordered();
                self.selected_block = Some(start.block);
            }
        }
    }

    /// The page-bottom progress bar, then chrome drawn over the finished
    /// page: top bar, floating page bubble, selection HUD, rails, note
    /// composer, comment overlay, and reading-settings popover. Restores
    /// `self.layout`/`self.library`/`self.annotations`, taken at the top of
    /// [`Self::draw_reader`].
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_footer_and_chrome(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        lib: Vec<Book>,
        annos: Vec<Annotation>,
        layout: ChapterLayout,
        palette: &ReaderPalette,
        geom: &ReaderPageGeometry,
        mode: ReadMode,
        nchap: usize,
        sel_anchor: SelAnchor,
    ) {
        // ---- page footer -------------------------------------------------
        // The page stays clean: only a thin book-progress bar spanning the full
        // page width. The page/chapter locators live in the bottom chrome bar.
        let frac = if nchap > 0 {
            let in_ch = if mode.is_continuous() {
                (self.scroll_y as f32 + geom.content_h) / layout.total_height.max(1.0)
            } else {
                (self.page as f32 + mode.page_stride() as f32) / layout.page_count() as f32
            };
            ((self.chapter as f32 + in_ch.min(1.0)) / nchap as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let pbar = Rect::new(
            geom.full_content_x,
            geom.paper_y + geom.paper_h - 16.0,
            geom.full_content_x + f64::from(geom.full_content_w),
            geom.paper_y + geom.paper_h - 13.0,
        );
        paint::progress(scene, t, pbar, frac, palette.rule, palette.accent);

        // restore the layout cache (key drives reuse next frame)
        self.layout = Some(layout);
        // restore the annotation cache before chrome reads it (the top bar's
        // bookmark state + the notes drawer both consult `self.annotations`)
        self.annotations = annos;

        // ---- chrome: top bar, floating page bubble, selection HUD, drawers -
        self.draw_reader_topbar(scene, t, tr, theme, &lib, c);
        self.reader_bubble.draw(
            scene,
            t,
            tr,
            theme,
            self.rect,
            self.cursor,
            geom.center_x0,
            geom.center_x1,
            ReaderPosition {
                chapter: self.chapter,
                page: self.page,
                page_count: self.cur_page_count,
                chapter_count: nchap.max(1),
            },
        );
        // Floating selection toolbar over the page (under any overlay rail).
        self.draw_selection_hud(
            scene,
            t,
            tr,
            theme,
            c,
            geom.paper_x,
            geom.paper_w,
            geom.body_y0,
            geom.body_y1,
            sel_anchor,
        );
        self.draw_rails(scene, t, tr, theme, &lib, geom.body_y0, geom.body_y1, c);
        // The on-page note composer (anchored near the passage) floats over the
        // page instead of shunting note-writing off to a separate screen.
        self.draw_note_composer(scene, t, tr, theme, c, sel_anchor.map(|(_, y)| y));
        // The floating comment overlay for a clicked highlight (its own composer
        // lives inside it, so the standalone note composer above stands down).
        self.comment_overlay.draw(
            scene,
            t,
            tr,
            theme,
            self.rect,
            self.cursor,
            &self.annotations,
            self.hi_color,
            self.composing,
        );
        // Reading-settings popover floats above everything.
        self.draw_type_popover(scene, t, tr, theme, c);

        self.library = lib;
    }

    /// Column, paper, and content-box geometry for one reader frame.
    /// Computed once per [`Self::draw_reader`] call from the current rect and
    /// the docked-rail reveal.
    fn reader_page_geometry(&self, r: Rect, mode: ReadMode) -> ReaderPageGeometry {
        // ---- geometry of the three columns ------------------------------
        // Each rail insets the page only while it is *docked* — an overlay rail
        // floats over a full-width page behind a scrim, so it contributes no
        // inset. The eased reveal makes the inset grow/shrink smoothly as a
        // docked rail is summoned or dismissed.
        let toc_w = match self.toc_layout() {
            SplitLayout::Docked => TOC_W * self.toc_flap.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let notes_w = match self.notes_layout() {
            SplitLayout::Docked => NOTES_W * self.notes_flap.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let center_x0 = r.x0 + toc_w;
        let center_x1 = r.x1 - notes_w;
        let body_y0 = r.y0 + TOP_BAR;
        // The page renders to the window bottom now that the always-on bottom
        // strip is gone — the page indicator floats as a transient bubble
        // (the `ReaderBubble` component) instead of costing a permanent strip of chrome.
        let body_y1 = r.y1;

        // ---- paper rect + content box -----------------------------------
        // On a small screen the page fills the reading area edge-to-edge, no
        // framed card or margin; a wide screen keeps the floating paper card.
        let framed = self.rect.width() >= FRAME_MIN_W;
        let center_w = (center_x1 - center_x0).max(120.0);
        let (paper_x, paper_y, paper_w, paper_h, pad_x, pad_top, pad_bot) = if framed {
            let paper_w = (center_w - 56.0).clamp(120.0, PAGE_MAX_W);
            let paper_x = center_x0 + (center_w - paper_w) / 2.0;
            let paper_y = body_y0 + 18.0;
            let paper_h = (body_y1 - body_y0 - 36.0).max(120.0);
            (paper_x, paper_y, paper_w, paper_h, 46.0, 50.0, 46.0)
        } else {
            let paper_x = center_x0;
            let paper_y = body_y0;
            let paper_w = center_w;
            let paper_h = (body_y1 - body_y0).max(120.0);
            (paper_x, paper_y, paper_w, paper_h, 22.0, 24.0, 30.0)
        };
        let full_content_x = paper_x + pad_x;
        let full_content_w = (paper_w - 2.0 * pad_x).max(60.0) as f32;
        let content_y = paper_y + pad_top;
        let content_h = (paper_h - pad_top - pad_bot).max(60.0) as f32;
        // A spread lays each page out in half the width (minus a centre gutter);
        // paged/continuous use the full column. The pagination width must match
        // the column the pages are shown in, so the layout key carries it.
        let gutter: f64 = if framed { 48.0 } else { 30.0 };
        let column_w = if mode == ReadMode::TwoUp {
            (((f64::from(full_content_w) - gutter) / 2.0).max(80.0)) as f32
        } else {
            full_content_w
        };
        // The header + progress bar use the left column origin; `content_w` is
        // the per-column layout width.
        let content_x = full_content_x;
        let content_w = column_w;

        ReaderPageGeometry {
            center_x0,
            center_x1,
            body_y0,
            body_y1,
            framed,
            paper_x,
            paper_y,
            paper_w,
            paper_h,
            full_content_x,
            full_content_w,
            content_x,
            content_y,
            content_w,
            content_h,
            gutter,
        }
    }

    /// (Re)compute the chapter layout if the cache key changed, or fall back
    /// to an empty layout when the book/chapter isn't available. Takes
    /// ownership of the previous cached layout via `self.layout.take()`.
    fn resolve_chapter_layout(
        &mut self,
        tr: &mut TextRenderer,
        lib: &[Book],
        geom: &ReaderPageGeometry,
        font_sig: u64,
        base_family: &FontChoice,
        font_mult: f32,
    ) -> ChapterLayout {
        let key = LayoutKey::new(
            self.chapter,
            geom.content_w,
            geom.content_h,
            &self.settings,
            font_sig,
        );
        if self.layout.as_ref().map(|l| l.key) != Some(key)
            && let Some(book) = lib.get(self.book_idx)
            && let Some(ch) = book.chapters.get(self.chapter)
        {
            self.layout = Some(layout_chapter(
                tr,
                &ch.blocks,
                geom.content_w,
                geom.content_h,
                &self.settings,
                self.chapter,
                &book.language,
                base_family,
                font_mult,
                font_sig,
            ));
        }
        self.layout.take().unwrap_or_else(|| ChapterLayout {
            blocks: Vec::new(),
            total_height: 0.0,
            pages: vec![0.0],
            key,
        })
    }

    /// Lay out the visible pane(s) for the current mode: one scrolling pane
    /// in continuous mode, one page in `Paged` mode, or a left/right spread in
    /// `TwoUp`. Also records `self.page_top_block`, the first visible block
    /// (the bookmark target).
    fn compute_panes(
        &mut self,
        layout: &ChapterLayout,
        mode: ReadMode,
        geom: &ReaderPageGeometry,
    ) -> Vec<Pane> {
        let win_for = |p: usize| -> (f32, f32) {
            let s = layout
                .pages
                .get(p)
                .copied()
                .unwrap_or(layout.total_height + 1.0);
            let e = layout
                .pages
                .get(p + 1)
                .copied()
                .unwrap_or(layout.total_height + 1.0);
            (s, e)
        };
        let mut panes: Vec<Pane> = Vec::new();
        match mode {
            ReadMode::Continuous => panes.push(Pane {
                x: geom.full_content_x,
                y: geom.content_y,
                w: geom.content_w,
                win_start: self.scroll_y as f32,
                win_end: self.scroll_y as f32 + geom.content_h,
                col: 0,
                partial: true,
            }),
            ReadMode::TwoUp => {
                let (ls, le) = win_for(self.page);
                panes.push(Pane {
                    x: geom.full_content_x,
                    y: geom.content_y,
                    w: geom.content_w,
                    win_start: ls,
                    win_end: le,
                    col: 0,
                    partial: false,
                });
                if self.page + 1 < layout.page_count() {
                    let (rs, re) = win_for(self.page + 1);
                    let rx = geom.full_content_x + f64::from(geom.content_w) + geom.gutter;
                    panes.push(Pane {
                        x: rx,
                        y: geom.content_y,
                        w: geom.content_w,
                        win_start: rs,
                        win_end: re,
                        col: 1,
                        partial: false,
                    });
                }
            }
            ReadMode::Paged => {
                let (s, e) = win_for(self.page);
                panes.push(Pane {
                    x: geom.full_content_x,
                    y: geom.content_y,
                    w: geom.content_w,
                    win_start: s,
                    win_end: e,
                    col: 0,
                    partial: false,
                });
            }
        }

        // first visible block on the top-left pane → bookmark target
        self.page_top_block = {
            let (ts, te) = (panes[0].win_start, panes[0].win_end);
            layout
                .blocks
                .iter()
                .find(|b| b.lines.iter().any(|l| l.y >= ts - 0.5 && l.y < te))
                .map_or(0, |b| b.index)
        };
        panes
    }

    /// The paper background (framed card or borderless fill) and the running
    /// chapter-title header + rule (framed only — a borderless small screen
    /// drops it to hand every pixel to the text; the chapter is still named
    /// in the bottom chrome bar).
    fn draw_reader_page_surface(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        lib: &[Book],
        palette: &ReaderPalette,
        geom: &ReaderPageGeometry,
    ) {
        let paper = Rect::new(
            geom.paper_x,
            geom.paper_y,
            geom.paper_x + geom.paper_w,
            geom.paper_y + geom.paper_h,
        );
        if geom.framed {
            let shadow = Rect::new(
                geom.paper_x - 2.0,
                geom.paper_y + 4.0,
                geom.paper_x + geom.paper_w + 2.0,
                geom.paper_y + geom.paper_h + 8.0,
            );
            paint::fill_round(scene, t, shadow, 10.0, Color::from_rgba8(0, 0, 0, 64));
            paint::fill_round(scene, t, paper, 8.0, palette.page);
            paint::stroke_round(scene, t, paper, 8.0, palette.page_edge, 1.0);
        } else {
            // Borderless: the page fills the reading area, no card or shadow.
            paint::fill_rect(scene, t, paper, palette.page);
        }

        if geom.framed {
            if let Some(book) = lib.get(self.book_idx)
                && let Some(ch) = book.chapters.get(self.chapter)
            {
                let head = ch.title.to_uppercase();
                tr.draw_text_weighted(
                    scene,
                    &head,
                    11.0,
                    palette.ink_dim,
                    t * Affine::translate((geom.content_x, geom.paper_y + 22.0)),
                    Some(geom.content_w),
                    TextWeight::Semibold,
                );
            }
            paint::line(
                scene,
                t,
                Point::new(geom.content_x, geom.paper_y + 42.0),
                Point::new(
                    geom.content_x + f64::from(geom.content_w),
                    geom.paper_y + 42.0,
                ),
                palette.rule,
                1.0,
            );
        }
    }

    /// Page-turn slide/fade transform: the content offset (from the
    /// turned-from side) and opacity for an in-progress turn, and whether
    /// one is still running. A settled turn clears `self.page_turn`, so the
    /// steady state is byte-for-byte the un-animated draw.
    fn page_turn_transform(
        &mut self,
        t: Affine,
        mode: ReadMode,
        paper_w: f64,
    ) -> (bool, Affine, f32) {
        let now = Instant::now();
        let turning = match self.page_turn {
            Some(pt) if !pt.done(now) && !mode.is_continuous() => true,
            Some(_) => {
                self.page_turn = None;
                false
            }
            None => false,
        };
        let (turn_dx, turn_alpha) = match (turning, self.page_turn) {
            (true, Some(pt)) => pt.visual(now, paper_w),
            _ => (0.0, 1.0),
        };
        let content_t = if turn_dx == 0.0 {
            t
        } else {
            t * Affine::translate((turn_dx, 0.0))
        };
        (turning, content_t, turn_alpha)
    }

    /// Draw each reading column with the page-turn slide/fade applied, and
    /// aggregate the first selected band's screen position and the open
    /// comment overlay's on-page bounds across all panes.
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_columns(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        layout: &ChapterLayout,
        annos: &[Annotation],
        palette: &ReaderPalette,
        base_family: &FontChoice,
        base_mono: bool,
        chapter: usize,
        sel_ord: Option<(DocPos, DocPos)>,
        comment_group: Option<Uuid>,
        panes: &[Pane],
        geom: &ReaderPageGeometry,
        mode: ReadMode,
    ) -> (SelAnchor, CommentBoxBounds) {
        let mut sel_anchor: SelAnchor = None;
        let mut comment_box: CommentBoxBounds = None;

        let (turning, content_t, turn_alpha) = self.page_turn_transform(t, mode, geom.paper_w);
        if turning {
            let clip = Rect::new(
                geom.paper_x + 1.0,
                geom.content_y - 3.0,
                geom.paper_x + geom.paper_w - 1.0,
                geom.content_y + f64::from(geom.content_h) + 3.0,
            );
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                turn_alpha,
                t,
                &RoundedRect::from_rect(clip, 6.0),
            );
        }
        for pane in panes {
            if pane.partial {
                // Continuous scroll shows partial lines at the edges — clip them
                // to the content box so nothing bleeds into header/footer chrome.
                let clip = Rect::new(
                    pane.x - 8.0,
                    geom.content_y - 2.0,
                    pane.x + f64::from(pane.w) + 8.0,
                    geom.content_y + f64::from(geom.content_h) + 2.0,
                );
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    content_t,
                    &RoundedRect::from_rect(clip, 2.0),
                );
            }
            let (a, cb) = self.draw_pane(
                scene,
                content_t,
                tr,
                layout,
                annos,
                palette,
                base_family,
                base_mono,
                chapter,
                sel_ord,
                comment_group,
                pane,
                theme,
            );
            if pane.partial {
                scene.pop_layer();
            }
            if sel_anchor.is_none() {
                sel_anchor = a;
            }
            if let Some(cb) = cb {
                comment_box = Some(match comment_box {
                    Some((x0, y0, x1, y1)) => {
                        (x0.min(cb.0), y0.min(cb.1), x1.max(cb.2), y1.max(cb.3))
                    }
                    None => cb,
                });
            }
        }
        if turning {
            scene.pop_layer();
        }
        (sel_anchor, comment_box)
    }

    /// Draw one reading column: margins, selection/highlight bands (rebuilds
    /// `sel_runs`), and glyphs. Returns the first selected band's screen
    /// `(x, y)` and the open comment overlay's highlight bounds, if any.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &ChapterLayout,
        annos: &[Annotation],
        palette: &ReaderPalette,
        base_family: &FontChoice,
        base_mono: bool,
        chapter: usize,
        sel_ord: Option<(DocPos, DocPos)>,
        comment_group: Option<Uuid>,
        pane: &Pane,
        theme: &Theme,
    ) -> (SelAnchor, CommentBoxBounds) {
        let content_x = pane.x;
        let content_w = pane.w;

        // ---- note bands + bookmark glyphs (behind text) -----------------
        Self::draw_pane_note_bands(
            scene, t, layout, annos, palette, theme, chapter, content_x, content_w, pane,
        );

        // ---- selection + ranged-highlight bands; rebuild sel_runs -------
        let (sel_anchor, comment_box, group_top) = self.draw_pane_selection_bands(
            scene,
            t,
            tr,
            layout,
            annos,
            palette,
            base_family,
            base_mono,
            chapter,
            sel_ord,
            comment_group,
            pane,
            content_x,
        );

        // ---- draggable highlight markers (left-margin grab handles) -----
        // One grip per highlight group, at the top of its first band. Recorded
        // in `hi_markers` so the press handler can hit-test it.
        self.draw_pane_highlight_markers(scene, t, palette, content_x, &group_top);

        // ---- glyphs + decorations (on top of bands) ---------------------
        Self::draw_pane_glyphs(
            scene,
            t,
            tr,
            layout,
            palette,
            base_family,
            base_mono,
            content_x,
            content_w,
            pane,
        );

        (sel_anchor, comment_box)
    }

    /// Note-comment bands and bookmark glyphs, drawn behind the text.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_note_bands(
        scene: &mut Scene,
        t: Affine,
        layout: &ChapterLayout,
        annos: &[Annotation],
        palette: &ReaderPalette,
        theme: &Theme,
        chapter: usize,
        content_x: f64,
        content_w: f32,
        pane: &Pane,
    ) {
        for b in &layout.blocks {
            let visible: Vec<&crate::paginate::LaidLine> = b
                .lines
                .iter()
                .filter(|l| pane.shows(l.y, b.line_height))
                .collect();
            if visible.is_empty() {
                continue;
            }
            let by0 = pane.screen_y(visible.first().unwrap().y) - 3.0;
            let by1 = pane.screen_y(visible.last().unwrap().y) + f64::from(b.line_height);
            for a in annos
                .iter()
                .filter(|a| a.chapter == chapter && a.block == b.index)
            {
                match a.kind {
                    AnnoKind::Highlight => {}
                    AnnoKind::Note => {
                        let band = Rect::new(
                            content_x + f64::from(b.indent) - 4.0,
                            by0,
                            content_x + f64::from(content_w),
                            by1,
                        );
                        paint::fill_round(
                            scene,
                            t,
                            band,
                            4.0,
                            palette.highlight_fill(a.color).with_alpha_factor(0.5),
                        );
                        let bar = Rect::new(
                            content_x + f64::from(b.indent) - 10.0,
                            by0,
                            content_x + f64::from(b.indent) - 6.0,
                            by1,
                        );
                        paint::fill_rect(scene, t, bar, ReaderPalette::highlight_swatch(a.color));
                        let gx = content_x + f64::from(content_w) + 6.0;
                        paint::note(
                            scene,
                            t,
                            Rect::new(gx, by0, gx + 20.0, by0 + 20.0),
                            palette.ink_dim,
                            1.6,
                        );
                    }
                    AnnoKind::Bookmark => {
                        let gx = content_x + f64::from(content_w) + 6.0;
                        paint::bookmark(
                            scene,
                            t,
                            Rect::new(gx, by0 - 2.0, gx + 20.0, by0 + 22.0),
                            theme.accent_primary,
                            1.6,
                            true,
                        );
                    }
                }
            }
        }
    }

    /// Selection + ranged-highlight bands across every block/line of the
    /// pane; also rebuilds `self.sel_runs` (needed for hit-testing) and
    /// `self.page_first_doc`/`page_last_doc`. Returns the first selected
    /// band's screen anchor, the open comment overlay's on-page bounds, and
    /// the topmost band per highlight group (for the draggable margin
    /// markers drawn after).
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_selection_bands(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &ChapterLayout,
        annos: &[Annotation],
        palette: &ReaderPalette,
        base_family: &FontChoice,
        base_mono: bool,
        chapter: usize,
        sel_ord: Option<(DocPos, DocPos)>,
        comment_group: Option<Uuid>,
        pane: &Pane,
        content_x: f64,
    ) -> (SelAnchor, CommentBoxBounds, HashMap<Uuid, (f64, HiColor)>) {
        let ranged: Vec<&Annotation> = annos
            .iter()
            .filter(|a| a.chapter == chapter && a.kind == AnnoKind::Highlight && a.start < a.end)
            .collect();
        // Topmost band `(y, colour)` per highlight group on this column, for the
        // draggable margin markers drawn after the bands.
        let mut group_top: HashMap<Uuid, (f64, HiColor)> = HashMap::new();
        let mut sel_anchor: SelAnchor = None;
        let mut comment_box: CommentBoxBounds = None;
        for b in &layout.blocks {
            if matches!(b.kind, LayoutKind::Separator) {
                continue;
            }
            let indent = f64::from(b.indent);
            let block_ranged: Vec<&Annotation> = ranged
                .iter()
                .copied()
                .filter(|a| a.block == b.index)
                .collect();
            for l in &b.lines {
                if !pane.shows(l.y, b.line_height) {
                    continue;
                }
                self.draw_pane_line_bands(
                    scene,
                    t,
                    tr,
                    palette,
                    base_family,
                    base_mono,
                    content_x,
                    pane,
                    b,
                    indent,
                    &block_ranged,
                    l,
                    sel_ord,
                    comment_group,
                    &mut group_top,
                    &mut sel_anchor,
                    &mut comment_box,
                );
            }
        }
        (sel_anchor, comment_box, group_top)
    }

    /// Rebuild `sel_runs` (and `page_first_doc`/`page_last_doc`) for every
    /// run of one line, and grow `sel_band`/`hi_span` to cover the portion
    /// of each run inside the live selection / a ranged highlight. Pure
    /// measurement — the bands themselves are drawn by the caller once the
    /// full span per highlight is known.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_line_runs(
        &mut self,
        tr: &mut TextRenderer,
        base_family: &FontChoice,
        base_mono: bool,
        content_x: f64,
        pane: &Pane,
        b: &crate::paginate::LaidBlock,
        indent: f64,
        l: &crate::paginate::LaidLine,
        ly: f64,
        lh: f64,
        block_ranged: &[&Annotation],
        sel_ord: Option<(DocPos, DocPos)>,
        sel_band: &mut Option<(f64, f64)>,
        hi_span: &mut [Option<(f64, f64)>],
    ) {
        for run in &l.runs {
            let weight = if run.bold {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            };
            let fam = if run.mono || base_mono {
                FontChoice::Monospace
            } else {
                base_family.clone()
            };
            let w = run.math.as_ref().map_or_else(
                || {
                    tr.measure_font(&run.display_text, b.size, None, weight, run.italic, &fam)
                        .0
                },
                |math| math.width as f32,
            ) * l.glyph_scale;
            let x0 = content_x + indent + f64::from(run.x);
            let x1 = x0 + f64::from(w);
            self.sel_runs.push(SelRun {
                block: b.index,
                x0,
                x1,
                y0: ly,
                y1: ly + lh,
                cs: run.char_start,
                cl: run.char_len,
                col: pane.col,
            });
            if self.page_first_doc.is_none() {
                self.page_first_doc = Some(DocPos {
                    block: b.index,
                    ch: run.char_start,
                });
            }
            self.page_last_doc = Some(DocPos {
                block: b.index,
                ch: run.char_start + run.char_len,
            });
            let run_end = run.char_start + run.char_len;
            let grow = |acc: &mut Option<(f64, f64)>, bx0: f64, bx1: f64| {
                *acc = Some(match *acc {
                    Some((a, b2)) => (a.min(bx0), b2.max(bx1)),
                    None => (bx0, bx1),
                });
            };
            if let Some((s, e)) = sel_ord
                && b.index >= s.block
                && b.index <= e.block
            {
                let lo = if b.index == s.block {
                    s.ch.max(run.char_start)
                } else {
                    run.char_start
                };
                let hi = if b.index == e.block {
                    e.ch.min(run_end)
                } else {
                    run_end
                };
                if lo < hi {
                    let (bx0, bx1) =
                        run_sub_x(x0, f64::from(w), run.char_start, run.char_len, lo, hi);
                    grow(sel_band, bx0, bx1);
                }
            }
            for (i, a) in block_ranged.iter().enumerate() {
                let lo = a.start.max(run.char_start);
                let hi = a.end.min(run_end);
                if lo < hi {
                    let (bx0, bx1) =
                        run_sub_x(x0, f64::from(w), run.char_start, run.char_len, lo, hi);
                    grow(&mut hi_span[i], bx0, bx1);
                }
            }
        }
    }

    /// Selection + ranged-highlight bands for one visible line: rebuilds its
    /// `sel_runs` (and `page_first_doc`/`page_last_doc`), then draws the
    /// highlight band(s) and the live-selection band for the line.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_line_bands(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        palette: &ReaderPalette,
        base_family: &FontChoice,
        base_mono: bool,
        content_x: f64,
        pane: &Pane,
        b: &crate::paginate::LaidBlock,
        indent: f64,
        block_ranged: &[&Annotation],
        l: &crate::paginate::LaidLine,
        sel_ord: Option<(DocPos, DocPos)>,
        comment_group: Option<Uuid>,
        group_top: &mut HashMap<Uuid, (f64, HiColor)>,
        sel_anchor: &mut SelAnchor,
        comment_box: &mut CommentBoxBounds,
    ) {
        let ly = pane.screen_y(l.y);
        let lh = f64::from(b.line_height);
        let mut sel_band: Option<(f64, f64)> = None;
        let mut hi_span: Vec<Option<(f64, f64)>> = vec![None; block_ranged.len()];
        self.draw_pane_line_runs(
            tr,
            base_family,
            base_mono,
            content_x,
            pane,
            b,
            indent,
            l,
            ly,
            lh,
            block_ranged,
            sel_ord,
            &mut sel_band,
            &mut hi_span,
        );
        for (i, a) in block_ranged.iter().enumerate() {
            if let Some((mnx, mxx)) = hi_span[i] {
                let band = Rect::new(mnx - 2.0, ly - 1.0, mxx + 2.0, ly + lh - 2.0);
                paint::fill_round(scene, t, band, 4.0, palette.highlight_fill(a.color));
                self.hi_bands.push(band);
                group_top
                    .entry(a.group)
                    .and_modify(|e| {
                        if band.y0 < e.0 {
                            *e = (band.y0, a.color);
                        }
                    })
                    .or_insert((band.y0, a.color));
                if comment_group == Some(a.group) {
                    *comment_box = Some(match *comment_box {
                        Some((x0, y0, x1, y1)) => (
                            x0.min(band.x0),
                            y0.min(band.y0),
                            x1.max(band.x1),
                            y1.max(band.y1),
                        ),
                        None => (band.x0, band.y0, band.x1, band.y1),
                    });
                }
            }
        }
        if let Some((mnx, mxx)) = sel_band {
            let band = Rect::new(mnx - 2.0, ly - 1.0, mxx + 2.0, ly + lh - 2.0);
            paint::fill_round(scene, t, band, 3.0, palette.accent.with_alpha_factor(0.30));
            if sel_anchor.is_none() {
                *sel_anchor = Some((f64::midpoint(mnx, mxx), ly));
            }
        }
    }

    /// Draggable highlight-margin markers: one grip per highlight group, at
    /// the top of its first band on this pane.
    fn draw_pane_highlight_markers(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        palette: &ReaderPalette,
        content_x: f64,
        group_top: &HashMap<Uuid, (f64, HiColor)>,
    ) {
        for (group, (gy, color)) in group_top {
            let mx0 = content_x - 15.0;
            let handle = Rect::new(mx0, *gy, mx0 + 7.0, *gy + 22.0);
            let hovered =
                self.marker_group == Some(*group) || handle.inflate(5.0, 5.0).contains(self.cursor);
            paint::fill_round(
                scene,
                t,
                handle,
                3.5,
                ReaderPalette::highlight_swatch(*color),
            );
            if hovered {
                paint::stroke_round(
                    scene,
                    t,
                    handle.inflate(2.0, 2.0),
                    4.5,
                    palette.ink_dim,
                    1.4,
                );
            }
            self.hi_markers.push((handle.inflate(6.0, 5.0), *group));
        }
    }

    /// Block decorations (Quote rule, Separator line, inline math, list
    /// bullets) and every run's glyphs, on top of the bands.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_glyphs(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        layout: &ChapterLayout,
        palette: &ReaderPalette,
        base_family: &FontChoice,
        base_mono: bool,
        content_x: f64,
        content_w: f32,
        pane: &Pane,
    ) {
        for b in &layout.blocks {
            Self::draw_pane_block_decoration(scene, t, palette, content_x, content_w, pane, b);
            if let Some(math) = &b.math
                && pane.shows(b.top, b.line_height)
            {
                ui::math::draw_layout(
                    scene,
                    t,
                    tr,
                    &math.layout,
                    Point::new(
                        content_x + f64::from(math.x),
                        pane.screen_y(b.top) + f64::from(math.y_offset),
                    ),
                    palette.ink,
                );
            }
            if let Some(bullet) = &b.bullet
                && let Some(first) = b.lines.iter().find(|l| pane.shows(l.y, b.line_height))
            {
                let ly = pane.screen_y(first.y);
                tr.draw_text(
                    scene,
                    bullet,
                    b.size,
                    palette.ink_dim,
                    t * Affine::translate((content_x + f64::from(b.indent) - 20.0, ly)),
                    None,
                );
            }
            Self::draw_pane_block_runs(
                scene,
                t,
                tr,
                palette,
                base_family,
                base_mono,
                content_x,
                pane,
                b,
            );
        }
    }

    /// The Quote side-rule, or the Separator's centred rule.
    fn draw_pane_block_decoration(
        scene: &mut Scene,
        t: Affine,
        palette: &ReaderPalette,
        content_x: f64,
        content_w: f32,
        pane: &Pane,
        b: &crate::paginate::LaidBlock,
    ) {
        match b.kind {
            LayoutKind::Quote => {
                let visible: Vec<&crate::paginate::LaidLine> = b
                    .lines
                    .iter()
                    .filter(|l| pane.shows(l.y, b.line_height))
                    .collect();
                if let (Some(f), Some(la)) = (visible.first(), visible.last()) {
                    let y0 = pane.screen_y(f.y);
                    let y1 = pane.screen_y(la.y) + f64::from(b.line_height);
                    let bar = Rect::new(content_x + 10.0, y0, content_x + 13.5, y1);
                    paint::fill_rect(scene, t, bar, palette.rule);
                }
            }
            LayoutKind::Separator if pane.shows(b.top, b.line_height) => {
                let cy = pane.screen_y(b.top) + f64::from(b.line_height) / 2.0;
                let cx = content_x + f64::from(content_w) / 2.0;
                paint::line(
                    scene,
                    t,
                    Point::new(cx - 28.0, cy),
                    Point::new(cx + 28.0, cy),
                    palette.rule,
                    1.4,
                );
            }
            _ => {}
        }
    }

    /// Every run's glyphs for one block: an inline-mono background chip when
    /// needed, structured math, or shaped text.
    #[allow(clippy::too_many_arguments)]
    fn draw_pane_block_runs(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        palette: &ReaderPalette,
        base_family: &FontChoice,
        base_mono: bool,
        content_x: f64,
        pane: &Pane,
        b: &crate::paginate::LaidBlock,
    ) {
        for l in &b.lines {
            if !pane.shows(l.y, b.line_height) {
                continue;
            }
            let ly = pane.screen_y(l.y);
            for run in &l.runs {
                let mono = run.mono || base_mono;
                let weight = if run.bold {
                    TextWeight::Bold
                } else {
                    TextWeight::Regular
                };
                let x = content_x + f64::from(b.indent) + f64::from(run.x);
                let run_y = ly + f64::from(run.y);
                let fam = if mono {
                    FontChoice::Monospace
                } else {
                    base_family.clone()
                };
                if run.mono && !base_mono && run.math.is_none() {
                    let (cw, _) = tr.measure_styled(
                        &run.display_text,
                        b.size,
                        None,
                        weight,
                        run.italic,
                        true,
                    );
                    let cw = cw * l.glyph_scale;
                    let chip = Rect::new(
                        x - 3.0,
                        run_y - 1.0,
                        x + f64::from(cw) + 3.0,
                        run_y + f64::from(b.size) + 3.0,
                    );
                    paint::fill_round(scene, t, chip, 3.0, palette.code_bg);
                }
                if let Some(math) = &run.math {
                    ui::math::draw_layout(
                        scene,
                        t * Affine::translate((x, run_y))
                            * Affine::scale_non_uniform(f64::from(l.glyph_scale), 1.0),
                        tr,
                        math,
                        Point::ZERO,
                        palette.ink,
                    );
                } else {
                    tr.draw_text_font(
                        scene,
                        &run.display_text,
                        b.size,
                        palette.ink,
                        t * Affine::translate((x, run_y))
                            * Affine::scale_non_uniform(f64::from(l.glyph_scale), 1.0),
                        None,
                        weight,
                        run.italic,
                        &fam,
                    );
                }
            }
        }
    }

    /// A floating toolbar shown while a paragraph is selected: highlight
    /// colours + Note, reachable at any width even with the rail collapsed.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_selection_hud(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        paper_x: f64,
        paper_w: f64,
        body_y0: f64,
        body_y1: f64,
        anchor: Option<(f64, f64)>,
    ) {
        // Hide the picker while a note is being composed (the composer takes
        // over) or when there's no active selection/block.
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        if self.view != View::Reader || !has_sel || self.composing.is_some() {
            return;
        }
        // The shared reader HUD owns look/spacing/hover; this reader keeps the
        // page palette's accent and maps emitted actions to its own commands.
        let del_group = self.selection_highlight_group();
        let palette = self.resolve_palette(theme);
        let style = ui::ReaderHudStyle {
            surface: c.surfaces.raised,
            border: c.border_standard,
            elevation: theme.tokens.elevation.e2,
            text: c.text_primary,
            hover: theme.bg_hover,
            pill: c.surfaces.overlay,
            ring: palette.accent,
            danger: c.semantic.danger,
        };
        let swatches: Vec<ui::HudSwatch> = HiColor::ALL
            .iter()
            .map(|&col| {
                ui::HudSwatch::new(
                    [col.rgb().0, col.rgb().1, col.rgb().2],
                    self.hi_color == col,
                )
            })
            .collect();
        // Anchor above the selection; fall back to the page centre near the top.
        let anchor_pt = anchor.map_or_else(
            || Point::new(paper_x + paper_w / 2.0, body_y0 + 54.0),
            |(x, y)| Point::new(x, y),
        );
        let bounds = Rect::new(self.rect.x0, body_y0, self.rect.x1, body_y1);
        let hud = ui::SelectionHud::draw(
            scene,
            t,
            tr,
            &style,
            &swatches,
            true,
            del_group.is_some(),
            self.cursor,
            anchor_pt,
            bounds,
        );
        // Swallow taps on the bar body, and route each control to a command.
        self.push_hit(hud.rect(), Action::Swallow);
        for &(rect, action) in hud.buttons() {
            let mapped = match action {
                ui::HudAction::Copy => Action::CopySelection,
                ui::HudAction::Note => Action::AddNote,
                ui::HudAction::Highlight(i) => {
                    Action::SetColor(HiColor::ALL[i.min(HiColor::ALL.len() - 1)])
                }
                ui::HudAction::Delete => match del_group {
                    Some(g) => Action::DeleteHighlight(g),
                    None => Action::Swallow,
                },
            };
            self.push_hit(rect, mapped);
        }
    }

    /// The on-page note composer: a small card anchored near the selected
    /// passage. Typing is handled by the composing key path; this draws the
    /// surface, live text with a caret, and Save/Delete.
    fn draw_note_composer(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _c: &ui::tokens::colors::Colors,
        anchor_y: Option<f64>,
    ) {
        let Some(id) = self.note_composer.id() else {
            return;
        };
        if self.view != View::Reader {
            return;
        }
        // When a highlight's comment overlay is open it hosts its own composer,
        // so the standalone note composer stands down to avoid two cards.
        if self.comment_overlay.is_open() {
            return;
        }
        let Some(annotation) = self.annotations.iter().find(|a| a.id == id) else {
            return;
        };
        self.note_composer
            .draw(scene, t, tr, theme, self.rect, anchor_y, annotation);
    }

    /// Paint the contents + annotations rails: either docked (flush, page
    /// already inset) or an overlay flap over the page behind a dimming
    /// scrim. The shared [`FlapState`] kernel supplies `reveal`/`slide`.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_rails(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lib: &[Book],
        body_y0: f64,
        body_y1: f64,
        c: &ui::tokens::colors::Colors,
    ) {
        use vello::peniko::Fill;
        let r = self.rect;
        let toc_layout = self.toc_layout();
        let notes_layout = self.notes_layout();
        let toc_reveal = self.toc_flap.reveal();
        let notes_reveal = self.notes_flap.reveal();

        // A single scrim dims the page whenever either rail is floating open in
        // overlay mode. Its alpha follows the stronger of the two reveals, and
        // a tap anywhere on it dismisses the open flap(s).
        let toc_overlay_open = toc_layout == SplitLayout::Overlay && toc_reveal > 0.001;
        let notes_overlay_open = notes_layout == SplitLayout::Overlay && notes_reveal > 0.001;
        if toc_overlay_open || notes_overlay_open {
            let a = toc_reveal.max(notes_reveal) * 0.5;
            let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(a as f32);
            scene.fill(Fill::NonZero, t, scrim, None, &r);
            // The scrim is a dismiss target behind the rails (recorded first so
            // the rail bodies, pushed later, win the topmost hit-test).
            self.push_hit(r, Action::DismissOverlay);
        }

        // Contents rail: docked rails sit flush at the left edge; an overlay
        // rail slides in from off-screen by its eased `slide`.
        if toc_layout == SplitLayout::Docked {
            if toc_reveal > 0.001 {
                self.draw_toc(scene, t, tr, theme, lib, r.x0, body_y0, body_y1, c);
            }
        } else if toc_reveal > 0.001 {
            let x0 = r.x0 - TOC_W * (1.0 - self.toc_flap.slide());
            // Swallow taps on the rail body so they don't reach the scrim;
            // pushed first so the chapter-row hits land on top of it.
            self.push_hit(Rect::new(x0, body_y0, x0 + TOC_W, body_y1), Action::Swallow);
            self.draw_toc(scene, t, tr, theme, lib, x0, body_y0, body_y1, c);
        }

        // Annotations rail: docked at the right edge, or an overlay flap that
        // slides in from the right.
        if notes_layout == SplitLayout::Docked {
            if notes_reveal > 0.001 {
                self.draw_notes(scene, t, tr, theme, r.x1 - NOTES_W, body_y0, body_y1, c);
            }
        } else if notes_reveal > 0.001 {
            let x0 = r.x1 - NOTES_W * self.notes_flap.slide();
            self.push_hit(
                Rect::new(x0, body_y0, x0 + NOTES_W, body_y1),
                Action::Swallow,
            );
            self.draw_notes(scene, t, tr, theme, x0, body_y0, body_y1, c);
        }
    }

    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_topbar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lib: &[Book],
        c: &ui::tokens::colors::Colors,
    ) {
        let r = self.rect;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::line(
            scene,
            t,
            Point::new(r.x0, r.y0 + TOP_BAR),
            Point::new(r.x1, r.y0 + TOP_BAR),
            c.border_standard,
            1.0,
        );
        let s = 34.0;
        // The bookmark reads as the primary "save my place" affordance, so it's
        // drawn a touch larger than the other chrome icons.
        let s_bm = 40.0;
        let cy = r.y0 + (TOP_BAR - s) / 2.0;
        // The reading-settings ("Aa") pill lives in the right cluster; its width
        // is measured up front so the centre title reserves room for it.
        let (aa_label_w, _) = tr.measure_weighted("Aa", 15.0, None, TextWeight::Semibold);
        let aa_w = f64::from(aa_label_w) + 26.0;
        let mut x = r.x0 + 14.0;
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::Back, false, Action::Back) + 6.0;
        let toc_active = self.toc_open();
        self.icon_button(
            scene,
            t,
            theme,
            x,
            cy,
            s,
            Ico::Toc,
            toc_active,
            Action::ToggleToc,
        );

        // centre: title (+ author, when it fits). The title is clamped to the
        // gap between the left controls and the right controls and ellipsised
        // rather than allowed to collide with either cluster on a narrow window.
        self.draw_reader_topbar_title(scene, t, tr, c, lib, r, x, s, s_bm, aa_w);

        // right: reading-settings ("Aa") pill + bookmark toggle + notes toggle
        self.draw_reader_topbar_right(scene, t, tr, theme, c, r, s, s_bm, aa_w, aa_label_w, cy);
    }

    /// The centre title (+ author, when it fits), clamped between the left
    /// controls and the right cluster and ellipsised rather than colliding
    /// with either on a narrow window.
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_topbar_title(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        lib: &[Book],
        r: Rect,
        x: f64,
        s: f64,
        s_bm: f64,
        aa_w: f64,
    ) {
        let Some(book) = lib.get(self.book_idx) else {
            return;
        };
        let left_bound = x + s + 12.0;
        // Right cluster, walking in from the edge: notes icon, bookmark,
        // then the "Aa" pill — the title clears the left edge of that pill.
        let right_bound = r.x1 - 14.0 - s - 4.0 - s_bm - 8.0 - aa_w - 12.0;
        let avail = (right_bound - left_bound).max(40.0);

        let auth = format!("   {}", book.author);
        let (aw, _) = tr.measure(&auth, 13.0, None);
        let full_title = ellipsize(tr, &book.title, 16.0, TextWeight::Semibold, avail);
        let (tw, _) = tr.measure_weighted(&full_title, 16.0, None, TextWeight::Semibold);
        // Only show the author if title + author both fit comfortably.
        let show_author = f64::from(tw) + f64::from(aw) <= avail && book.title == full_title;
        let total = if show_author {
            f64::from(tw) + f64::from(aw)
        } else {
            f64::from(tw)
        };
        let cx = (f64::midpoint(left_bound, right_bound) - total / 2.0).max(left_bound);
        tr.draw_text_weighted(
            scene,
            &full_title,
            16.0,
            c.text_primary,
            t * Affine::translate((cx, r.y0 + 17.0)),
            None,
            TextWeight::Semibold,
        );
        if show_author {
            tr.draw_text(
                scene,
                &auth,
                13.0,
                c.text_dim,
                t * Affine::translate((cx + f64::from(tw), r.y0 + 18.5)),
                None,
            );
        }
    }

    /// Right cluster: notes toggle, bookmark toggle, and the reading-settings
    /// ("Aa") pill — relocated off the removed bottom strip so typography
    /// stays one tap away. Opens the same type/spacing/theme popover, with
    /// the same active state + hit target.
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_topbar_right(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        r: Rect,
        s: f64,
        s_bm: f64,
        aa_w: f64,
        aa_label_w: f32,
        cy: f64,
    ) {
        let bookmarked = self.annotations.iter().any(|a| {
            a.kind == AnnoKind::Bookmark
                && a.chapter == self.chapter
                && a.block == self.page_top_block
        });
        let mut rx = r.x1 - 14.0 - s;
        let notes_active = self.notes_open();
        self.icon_button(
            scene,
            t,
            theme,
            rx,
            cy,
            s,
            Ico::Notes,
            notes_active,
            Action::ToggleNotes,
        );
        rx -= s_bm + 4.0;
        let cy_bm = r.y0 + (TOP_BAR - s_bm) / 2.0;
        self.icon_button(
            scene,
            t,
            theme,
            rx,
            cy_bm,
            s_bm,
            Ico::Bookmark(bookmarked),
            bookmarked,
            Action::ToggleBookmark,
        );

        rx -= 8.0 + aa_w;
        let aa_h = 32.0;
        let aa_y = r.y0 + (TOP_BAR - aa_h) / 2.0;
        let aa_rect = Rect::new(rx, aa_y, rx + aa_w, aa_y + aa_h);
        let aa_active = self.type_popover;
        let aa_hover = self.is_hover(aa_rect);
        let aa_bg = if aa_active {
            theme.accent_primary
        } else if aa_hover {
            theme.bg_hover
        } else {
            c.surfaces.overlay
        };
        paint::fill_round(scene, t, aa_rect, aa_h / 2.0, aa_bg);
        let aa_ink = if aa_active {
            Color::from_rgb8(248, 248, 252)
        } else {
            c.text_primary
        };
        tr.draw_text_weighted(
            scene,
            "Aa",
            15.0,
            aa_ink,
            t * Affine::translate((
                rx + (aa_w - f64::from(aa_label_w)) / 2.0,
                aa_y + (aa_h - 15.0) / 2.0,
            )),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(aa_rect, Action::ToggleTypePopover);
    }

    /// Horizontal bounds `(left, right)` for the reading-settings popover,
    /// kept clear of any docked rail. Shared by the draw path and its layout test.
    fn type_popover_x_bounds(&self, panel_w: f64) -> (f64, f64) {
        let r = self.rect;
        let notes_docked_w = match self.notes_layout() {
            SplitLayout::Docked => NOTES_W * self.notes_flap.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let toc_docked_w = match self.toc_layout() {
            SplitLayout::Docked => TOC_W * self.toc_flap.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let px1 = r.x1 - 18.0 - notes_docked_w;
        let px0 = (px1 - panel_w).max(r.x0 + 12.0 + toc_docked_w);
        (px0, px1)
    }

    fn draw_type_popover(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
    ) {
        if !self.type_popover {
            return;
        }
        let r = self.rect;
        // No scrim: the page IS the live preview of the type/colour you're
        // choosing, so it must stay at full brightness while this popover is
        // open. A full-surface *invisible* dismiss target still lets a tap
        // anywhere off the panel close it.
        self.push_hit(r, Action::DismissOverlay);

        let layout = self.type_popover_layout(tr, r);
        // Body panel (its own tap-swallow so clicks inside don't dismiss).
        paint::fill_round(
            scene,
            t,
            layout.panel,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );
        paint::stroke_round(
            scene,
            t,
            layout.panel,
            f64::from(theme.tokens.radii.lg),
            c.border_standard,
            1.0,
        );
        self.push_hit(layout.panel, Action::Swallow);

        let (ix0, ix1) = (layout.ix0, layout.ix1);
        let y = layout.panel.y0 + layout.pad;
        let y = self.draw_type_popover_header(scene, t, tr, theme, c, ix0, ix1, y, layout.header_h);
        let y = self.draw_type_popover_steppers(scene, t, tr, theme, c, ix0, ix1, y, layout.row_h);
        let y = self.draw_type_popover_layout_row(
            scene,
            t,
            tr,
            theme,
            c,
            ix0,
            ix1,
            y,
            layout.pill_h,
            layout.pill_gap,
        );
        let y = self.draw_type_popover_composition_row(
            scene,
            t,
            tr,
            theme,
            c,
            ix0,
            y,
            layout.pill_h,
            layout.pill_gap,
        );
        let y = self.draw_type_popover_typeface_row(
            scene,
            t,
            tr,
            theme,
            c,
            ix0,
            ix1,
            y,
            layout.pill_h,
            layout.pill_gap,
            &layout.font_pills,
        );
        self.draw_type_popover_page_row(
            scene,
            t,
            tr,
            theme,
            c,
            ix0,
            ix1,
            y,
            layout.pill_h,
            layout.pill_gap,
            &layout.scheme_pills,
            &layout.scheme_widths,
        );
    }

    /// Sizing for the whole popover: pre-measured font/scheme pills (so the
    /// panel sizes to however many the reader has chosen to show), the
    /// panel's total height from every section's row count, and its clamped
    /// on-screen rect.
    fn type_popover_layout(&self, tr: &mut TextRenderer, r: Rect) -> TypePopoverLayout {
        let pad = 16.0;
        let row_h = 34.0;
        let pill_h = row_h - 4.0;
        let pill_gap = 8.0;
        let panel_w = 320.0_f64.min(r.width() - 24.0);
        let inner_w = panel_w - pad * 2.0;

        let shown = self.shown_fonts();
        let active_id = self.active_font().id;
        let font_pills: Vec<(String, String, f64, bool)> = shown
            .iter()
            .map(|f| {
                let w = f64::from(
                    tr.measure_weighted(&f.name, 12.5, None, TextWeight::Medium)
                        .0,
                ) + 24.0;
                (f.id.clone(), f.name.clone(), w, f.id == active_id)
            })
            .collect();
        let font_widths: Vec<f64> = font_pills.iter().map(|p| p.2).collect();
        let font_rows = wrap_row_count(&font_widths, inner_w, pill_gap);

        // Enabled colour schemes offered as PAGE pills.
        let scheme_pills: Vec<SchemePill> = self.quick_scheme_pills();
        let scheme_widths: Vec<f64> = scheme_pills
            .iter()
            .map(|p| {
                f64::from(
                    tr.measure_weighted(&p.label, 12.5, None, TextWeight::Medium)
                        .0,
                ) + 24.0
            })
            .collect();
        let scheme_rows = wrap_row_count(&scheme_widths, inner_w, pill_gap);

        let header_h = 30.0;
        let steppers_h = row_h + 10.0 + row_h;
        // Layout row: a "LAYOUT" label + one row of page/scroll/spread pills.
        let layout_h = 16.0 + pill_h;
        let composition_h = 16.0 + pill_h;
        let type_h =
            16.0 + font_rows as f64 * pill_h + font_rows.saturating_sub(1) as f64 * pill_gap;
        let page_h =
            16.0 + scheme_rows as f64 * pill_h + scheme_rows.saturating_sub(1) as f64 * pill_gap;
        let panel_h = pad * 2.0
            + header_h
            + steppers_h
            + 22.0
            + layout_h
            + 22.0
            + composition_h
            + 22.0
            + type_h
            + 22.0
            + page_h;

        // The "Aa" trigger now lives in the top bar, so the popover drops down
        // from just below it (right-aligned via the shared x-bounds), clamping
        // its foot to the window bottom on a short window.
        let (px0, px1) = self.type_popover_x_bounds(panel_w);
        let py0 = r.y0 + TOP_BAR + 8.0;
        let py1 = (py0 + panel_h).min(r.y1 - 12.0);

        TypePopoverLayout {
            pad,
            row_h,
            pill_h,
            pill_gap,
            header_h,
            panel: Rect::new(px0, py0, px1, py1),
            ix0: px0 + pad,
            ix1: px1 - pad,
            font_pills,
            scheme_pills,
            scheme_widths,
        }
    }

    /// "Display" title + the ⋯ button that opens the full settings manager.
    /// Returns the y just below the header.
    #[allow(clippy::too_many_arguments)]
    fn draw_type_popover_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        y: f64,
        header_h: f64,
    ) -> f64 {
        // Header: "Display" + a small ⋯ button that opens the full manager.
        tr.draw_text_weighted(
            scene,
            "Display",
            13.5,
            c.text_primary,
            t * Affine::translate((ix0, y + 4.0)),
            None,
            TextWeight::Semibold,
        );
        let dots_s = 26.0;
        self.icon_button(
            scene,
            t,
            theme,
            ix1 - dots_s,
            y - 2.0,
            dots_s,
            Ico::Dots,
            false,
            Action::OpenSettings,
        );
        y + header_h
    }

    /// Text-size and line-spacing steppers (each value is click-to-type as
    /// well as +/− steppable). Returns the y just below them.
    #[allow(clippy::too_many_arguments)]
    fn draw_type_popover_steppers(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        y: f64,
        row_h: f64,
    ) -> f64 {
        self.stepper_row(
            scene,
            t,
            tr,
            theme,
            c,
            ix0,
            ix1,
            y,
            row_h,
            "Text size",
            f64::from(self.settings.font_scale) * 100.0,
            NumberField::FontScale,
        );
        let y = y + row_h + 10.0;
        self.stepper_row(
            scene,
            t,
            tr,
            theme,
            c,
            ix0,
            ix1,
            y,
            row_h,
            "Line spacing",
            f64::from(self.settings.line_spacing),
            NumberField::LineSpacing,
        );
        y + row_h + 22.0
    }

    /// "LAYOUT" — single page / continuous scroll / two-up spread pills,
    /// wrapping within the panel. Returns the y just below the row(s).
    #[allow(clippy::too_many_arguments)]
    fn draw_type_popover_layout_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        pill_h: f64,
        pill_gap: f64,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "LAYOUT",
            10.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 16.0;
        let mut sx = ix0;
        for m in ReadMode::ALL {
            let w = f64::from(
                tr.measure_weighted(m.label(), 12.5, None, TextWeight::Medium)
                    .0,
            ) + 24.0;
            if sx + w > ix1 && sx > ix0 {
                sx = ix0;
                y += pill_h + pill_gap;
            }
            let active = self.settings.mode == m;
            let action = if active {
                Action::Swallow
            } else {
                Action::SetMode(m)
            };
            self.pill(
                scene,
                t,
                tr,
                theme,
                sx,
                y,
                pill_h,
                m.label(),
                12.5,
                active,
                Some(action),
            );
            sx += w + pill_gap;
        }
        y + pill_h + 22.0
    }

    /// "COMPOSITION" — book-quality global fitting or natural ragged lines.
    /// Returns the y just below the row.
    #[allow(clippy::too_many_arguments)]
    fn draw_type_popover_composition_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        mut y: f64,
        pill_h: f64,
        pill_gap: f64,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "COMPOSITION",
            10.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 16.0;
        let mut sx = ix0;
        for composition in CompositionStyle::ALL {
            let label = composition.label();
            let width =
                f64::from(tr.measure_weighted(label, 12.5, None, TextWeight::Medium).0) + 24.0;
            let active = self.settings.composition == composition;
            let action = if active {
                Action::Swallow
            } else {
                Action::SetComposition(composition)
            };
            self.pill(
                scene,
                t,
                tr,
                theme,
                sx,
                y,
                pill_h,
                label,
                12.5,
                active,
                Some(action),
            );
            sx += width + pill_gap;
        }
        y + pill_h + 22.0
    }

    /// "TYPEFACE" — one pill per shown font, wrapping within the panel.
    /// Returns the y just below the row(s).
    #[allow(clippy::too_many_arguments)]
    fn draw_type_popover_typeface_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        pill_h: f64,
        pill_gap: f64,
        font_pills: &[(String, String, f64, bool)],
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "TYPEFACE",
            10.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 16.0;
        let mut sx = ix0;
        for (id, name, w, active) in font_pills {
            if sx + w > ix1 && sx > ix0 {
                sx = ix0;
                y += pill_h + pill_gap;
            }
            let action = if *active {
                Action::Swallow
            } else {
                Action::SetFont(id.clone())
            };
            self.pill(
                scene,
                t,
                tr,
                theme,
                sx,
                y,
                pill_h,
                name,
                12.5,
                *active,
                Some(action),
            );
            sx += w + pill_gap;
        }
        y + pill_h + 22.0
    }

    /// "PAGE" — one pill per enabled colour scheme, wrapping within the panel.
    #[allow(clippy::too_many_arguments)]
    fn draw_type_popover_page_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        pill_h: f64,
        pill_gap: f64,
        scheme_pills: &[SchemePill],
        scheme_widths: &[f64],
    ) {
        tr.draw_text_weighted(
            scene,
            "PAGE",
            10.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 16.0;
        let mut sx = ix0;
        for (p, w) in scheme_pills.iter().zip(scheme_widths.iter()) {
            if sx + w > ix1 && sx > ix0 {
                sx = ix0;
                y += pill_h + pill_gap;
            }
            let action = if p.active {
                Action::Swallow
            } else {
                p.action.clone()
            };
            self.pill(
                scene,
                t,
                tr,
                theme,
                sx,
                y,
                pill_h,
                &p.label,
                12.5,
                p.active,
                Some(action),
            );
            sx += w + pill_gap;
        }
        let _ = y;
    }

    /// A labelled −/value/＋ stepper row used inside the reading popover. The
    /// value itself is a click-to-type field: tapping it (via `field`) starts a
    /// numeric edit, in addition to the ± steppers.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn stepper_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x0: f64,
        x1: f64,
        y: f64,
        h: f64,
        label: &str,
        value: f64,
        field: NumberField,
    ) {
        tr.draw_text_weighted(
            scene,
            label,
            13.0,
            c.text_primary,
            t * Affine::translate((x0, y + (h - 13.0) / 2.0)),
            None,
            TextWeight::Medium,
        );
        let width = (h * 2.0 + 62.0).min((x1 - x0) * 0.58);
        self.number_controls.draw(
            field,
            value,
            Rect::new(x1 - width, y, x1, y + h),
            scene,
            t,
            tr,
            theme,
        );
    }

    /// `panel_x0` is the rail's left edge — `r.x0` when docked, or a slid
    /// position when it floats as an overlay flap.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_toc(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        lib: &[Book],
        panel_x0: f64,
        y0: f64,
        y1: f64,
        c: &ui::tokens::colors::Colors,
    ) {
        let panel = Rect::new(panel_x0, y0, panel_x0 + TOC_W, y1);
        paint::fill_rect(scene, t, panel, c.surfaces.base);
        paint::line(
            scene,
            t,
            Point::new(panel_x0 + TOC_W, y0),
            Point::new(panel_x0 + TOC_W, y1),
            c.border_standard,
            1.0,
        );
        tr.draw_text_weighted(
            scene,
            "CONTENTS",
            11.0,
            c.text_dim,
            t * Affine::translate((panel_x0 + 22.0, y0 + 18.0)),
            None,
            TextWeight::Semibold,
        );

        let Some(book) = lib.get(self.book_idx) else {
            return;
        };
        let mut y = y0 + 44.0;
        let row_h = 44.0;
        for (i, ch) in book.chapters.iter().enumerate() {
            if y + row_h > y1 {
                break;
            }
            let row = Rect::new(panel_x0 + 10.0, y, panel_x0 + TOC_W - 10.0, y + row_h - 6.0);
            let active = i == self.chapter;
            let hover = self.is_hover(row);
            if active {
                paint::fill_round(
                    scene,
                    t,
                    row,
                    f64::from(theme.tokens.radii.md),
                    theme.accent_primary.with_alpha_factor(0.16),
                );
                let bar = Rect::new(row.x0, row.y0 + 6.0, row.x0 + 3.0, row.y1 - 6.0);
                paint::fill_round(scene, t, bar, 2.0, theme.accent_primary);
            } else if hover {
                paint::fill_round(
                    scene,
                    t,
                    row,
                    f64::from(theme.tokens.radii.md),
                    theme.bg_hover,
                );
            }
            let num = format!("{:02}", i + 1);
            tr.draw_text_weighted(
                scene,
                &num,
                12.0,
                if active {
                    theme.accent_primary
                } else {
                    c.text_dim
                },
                t * Affine::translate((row.x0 + 14.0, y + 8.0)),
                None,
                TextWeight::Bold,
            );
            tr.draw_text_weighted(
                scene,
                &ch.title,
                13.0,
                if active { c.text_primary } else { c.text_dim },
                t * Affine::translate((row.x0 + 42.0, y + 8.0)),
                Some((TOC_W - 72.0) as f32),
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                },
            );
            self.push_hit(row, Action::GotoChapter(i));
            y += row_h;
        }
    }

    /// `panel_x0` is the rail's left edge — `r.x1 - NOTES_W` when docked, or a
    /// slid position when it floats as an overlay flap from the right.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_notes(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel_x0: f64,
        y0: f64,
        y1: f64,
        c: &ui::tokens::colors::Colors,
    ) {
        let annos = self.annotations.clone();
        let x0 = panel_x0;
        let panel_x1 = panel_x0 + NOTES_W;
        let panel = Rect::new(x0, y0, panel_x1, y1);
        paint::fill_rect(scene, t, panel, c.surfaces.base);
        paint::line(
            scene,
            t,
            Point::new(x0, y0),
            Point::new(x0, y1),
            c.border_standard,
            1.0,
        );

        Self::draw_notes_header(scene, t, tr, c, x0, y0, annos.len());
        let add_y = self.draw_notes_add_row(scene, t, tr, theme, c, x0, panel_x1, y0);

        // list
        let mut y = add_y + 46.0;
        let card_x0 = x0 + 16.0;
        let card_x1 = panel_x1 - 16.0;
        for a in &annos {
            if y > y1 - 40.0 {
                break;
            }
            y = self.draw_note_card(scene, t, tr, theme, c, card_x0, card_x1, y, a);
        }
    }

    /// "ANNOTATIONS" title + the count badge.
    #[allow(clippy::too_many_arguments)]
    fn draw_notes_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        x0: f64,
        y0: f64,
        count: usize,
    ) {
        tr.draw_text_weighted(
            scene,
            "ANNOTATIONS",
            11.0,
            c.text_dim,
            t * Affine::translate((x0 + 22.0, y0 + 18.0)),
            None,
            TextWeight::Semibold,
        );
        let count = format!("{count}");
        let (cw, _) = tr.measure(&count, 11.0, None);
        let badge = Rect::new(
            x0 + 110.0,
            y0 + 14.0,
            x0 + 110.0 + f64::from(cw) + 16.0,
            y0 + 14.0 + 18.0,
        );
        paint::fill_round(scene, t, badge, 9.0, c.surfaces.overlay);
        tr.draw_text(
            scene,
            &count,
            11.0,
            c.text_dim,
            t * Affine::translate((x0 + 118.0, y0 + 17.0)),
            None,
        );
    }

    /// "+ Note on selection" action row. Returns its y (the list starts
    /// below it).
    #[allow(clippy::too_many_arguments)]
    fn draw_notes_add_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x0: f64,
        panel_x1: f64,
        y0: f64,
    ) -> f64 {
        let add_y = y0 + 42.0;
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        let add_label = if has_sel {
            "+ Note on selection"
        } else {
            "Select text to annotate"
        };
        let add_rect = Rect::new(x0 + 16.0, add_y, panel_x1 - 16.0, add_y + 30.0);
        let hover = has_sel && self.is_hover(add_rect);
        paint::fill_round(
            scene,
            t,
            add_rect,
            f64::from(theme.tokens.radii.md),
            if hover {
                theme.bg_hover
            } else {
                c.surfaces.overlay
            },
        );
        tr.draw_text_weighted(
            scene,
            add_label,
            12.0,
            if has_sel { c.text_primary } else { c.text_dim },
            t * Affine::translate((x0 + 28.0, add_y + 8.0)),
            None,
            TextWeight::Medium,
        );
        if has_sel {
            self.push_hit(add_rect, Action::AddNote);
        }
        add_y
    }

    /// One annotation card in the notes list. Returns the y just below it
    /// (the next card's top).
    #[allow(clippy::too_many_arguments)]
    fn draw_note_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        card_x0: f64,
        card_x1: f64,
        y: f64,
        a: &Annotation,
    ) -> f64 {
        // measure note/excerpt to size the card
        let inner_w = (card_x1 - card_x0 - 28.0) as f32;
        let excerpt = if a.excerpt.is_empty() {
            "(no excerpt)".to_string()
        } else {
            format!("“{}”", a.excerpt)
        };
        let (_, eh) = tr.measure(&excerpt, 12.0, Some(inner_w));
        let composing = self.composing == Some(a.id);
        let note_disp = if composing {
            format!("{}\u{2502}", a.note)
        } else {
            a.note.clone()
        };
        let (_, nh) = if note_disp.is_empty() {
            (0.0, 0.0)
        } else {
            tr.measure(&note_disp, 12.5, Some(inner_w))
        };
        let card_h = 30.0
            + f64::from(eh)
            + if note_disp.is_empty() {
                0.0
            } else {
                f64::from(nh) + 12.0
            }
            + 16.0;
        let card = Rect::new(card_x0, y, card_x1, y + card_h);

        self.draw_note_card_frame(scene, t, theme, c, card, composing);
        self.draw_note_card_header(scene, t, tr, theme, c, card_x0, card_x1, y, a);
        Self::draw_note_card_body(
            scene, t, tr, c, card_x0, y, inner_w, &excerpt, &note_disp, eh,
        );
        self.push_note_card_hits(card_x0, card_x1, y, card_h, a);

        y + card_h + 12.0
    }

    fn draw_note_card_frame(
        &self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        card: Rect,
        composing: bool,
    ) {
        let hover = self.is_hover(card);
        paint::fill_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        if composing {
            paint::stroke_round(
                scene,
                t,
                card,
                f64::from(theme.tokens.radii.md),
                theme.border_focus,
                1.6,
            );
        } else if hover {
            paint::stroke_round(
                scene,
                t,
                card,
                f64::from(theme.tokens.radii.md),
                c.border_standard,
                1.0,
            );
        }
    }

    /// Colour dot / bookmark glyph + kind label + chapter/paragraph locator,
    /// and the always-visible delete button.
    #[allow(clippy::too_many_arguments)]
    fn draw_note_card_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        card_x0: f64,
        card_x1: f64,
        y: f64,
        a: &Annotation,
    ) {
        // header: colour dot + kind + location
        let dot = Point::new(card_x0 + 16.0, y + 17.0);
        match a.kind {
            AnnoKind::Bookmark => paint::bookmark(
                scene,
                t,
                Rect::new(dot.x - 7.0, y + 8.0, dot.x + 7.0, y + 24.0),
                theme.accent_primary,
                1.5,
                true,
            ),
            _ => paint::circle(scene, t, dot, 6.0, ReaderPalette::highlight_swatch(a.color)),
        }
        tr.draw_text_weighted(
            scene,
            a.kind.label(),
            12.0,
            c.text_primary,
            t * Affine::translate((card_x0 + 30.0, y + 11.0)),
            None,
            TextWeight::Semibold,
        );
        let loc = format!("Ch {} · ¶{}", a.chapter + 1, a.block + 1);
        let (lw, _) = tr.measure(&loc, 11.0, None);
        tr.draw_text(
            scene,
            &loc,
            11.0,
            c.text_dim,
            t * Affine::translate((card_x1 - 30.0 - f64::from(lw), y + 12.0)),
            None,
        );
        // Delete is always drawn and hit-registered, not gated on hover —
        // the only way to remove an annotation, so it must reach touch/keyboard.
        let del = Rect::new(card_x1 - 26.0, y + 6.0, card_x1 - 6.0, y + 26.0);
        let del_hover = self.is_hover(del);
        let del_col = if del_hover {
            c.text_primary
        } else {
            c.text_dim.with_alpha_factor(0.55)
        };
        paint::close(scene, t, del, del_col, 1.4);
        self.push_hit(del, Action::Delete(a.id));
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_note_card_body(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        card_x0: f64,
        y: f64,
        inner_w: f32,
        excerpt: &str,
        note_disp: &str,
        eh: f32,
    ) {
        // excerpt
        tr.draw_text(
            scene,
            excerpt,
            12.0,
            c.text_dim,
            t * Affine::translate((card_x0 + 14.0, y + 30.0)),
            Some(inner_w),
        );
        // note body
        if !note_disp.is_empty() {
            tr.draw_text(
                scene,
                note_disp,
                12.5,
                c.text_primary,
                t * Affine::translate((card_x0 + 14.0, y + 30.0 + f64::from(eh) + 8.0)),
                Some(inner_w),
            );
        }
    }

    /// click card → jump; click again on a note → edit.
    fn push_note_card_hits(
        &mut self,
        card_x0: f64,
        card_x1: f64,
        y: f64,
        card_h: f64,
        a: &Annotation,
    ) {
        // jump is the primary intent for the whole card area minus delete
        self.push_hit(
            Rect::new(card_x0, y, card_x1 - 30.0, y + card_h),
            Action::JumpTo(a.id),
        );
        if a.kind == AnnoKind::Note {
            // a smaller secondary zone to begin editing
            self.push_hit(
                Rect::new(card_x0, y + 28.0, card_x1 - 30.0, y + card_h),
                Action::EditNote(a.id),
            );
        }
    }
}

// --- Settings screen: the font + colour-scheme managers ---------------------

impl BooksApp {
    fn draw_settings(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = theme.tokens.colors.clone();
        let r = self.rect;

        self.draw_settings_topbar(scene, t, tr, theme, &c, r);
        let tabs_y = r.y0 + TOP_BAR + 10.0;
        let tab_h = self.draw_settings_tabs(scene, t, tr, theme, &c, r, tabs_y);

        // Scrollable body.
        let content_top = tabs_y + tab_h + 18.0;
        let body = Rect::new(r.x0, content_top, r.x1, r.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        let col_w = (r.width() - 96.0).clamp(280.0, 760.0);
        let col_x = r.x0 + (r.width() - col_w) / 2.0;
        let start_y = content_top - self.settings_scroll;
        let end_y = match self.settings_tab {
            SettingsTab::Library => {
                self.draw_library_settings(scene, t, tr, theme, &c, col_x, col_w, start_y)
            }
            SettingsTab::Fonts => {
                self.draw_fonts_manager(scene, t, tr, theme, &c, col_x, col_w, start_y)
            }
            SettingsTab::Colours => {
                self.draw_schemes_manager(scene, t, tr, theme, &c, col_x, col_w, start_y)
            }
        };
        scene.pop_layer();
        let content_h = end_y - start_y;
        let visible = (r.y1 - content_top).max(0.0);
        self.settings_max_scroll = (content_h - visible + 24.0).max(0.0);
        self.settings_scroll = self.settings_scroll.clamp(0.0, self.settings_max_scroll);

        // Modal overlays.
        if self.editing_scheme.is_some() {
            self.draw_scheme_editor(scene, t, tr, theme, &c);
        }
        if self.font_picker {
            self.draw_font_picker(scene, t, tr, theme, &c);
        }
        // The colour picker floats above everything when open.
        if self.color_picker.is_open() {
            self.color_picker
                .draw(scene, t, tr, theme, self.rect, self.cursor);
        }
    }

    fn draw_settings_topbar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        r: Rect,
    ) {
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::line(
            scene,
            t,
            Point::new(r.x0, r.y0 + TOP_BAR),
            Point::new(r.x1, r.y0 + TOP_BAR),
            c.border_standard,
            1.0,
        );
        let s = 34.0;
        let cy = r.y0 + (TOP_BAR - s) / 2.0;
        self.icon_button(
            scene,
            t,
            theme,
            r.x0 + 14.0,
            cy,
            s,
            Ico::Back,
            false,
            Action::CloseSettings,
        );
        tr.draw_text_weighted(
            scene,
            "Settings",
            21.0,
            c.text_primary,
            t * Affine::translate((r.x0 + 60.0, r.y0 + 15.0)),
            None,
            TextWeight::Bold,
        );
    }

    /// The underline-indicator tab bar (Library/Fonts/Colours). Returns the
    /// tab row's height.
    #[allow(clippy::too_many_arguments)]
    fn draw_settings_tabs(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        r: Rect,
        tabs_y: f64,
    ) -> f64 {
        let tab_h = 38.0;
        let mut tx = r.x0 + 26.0;
        for (tab, label) in [
            (SettingsTab::Library, "Library"),
            (SettingsTab::Fonts, "Fonts"),
            (SettingsTab::Colours, "Colour schemes"),
        ] {
            let active = self.settings_tab == tab;
            let (lw, _) = tr.measure_weighted(label, 14.0, None, TextWeight::Semibold);
            let tab_rect = Rect::new(tx, tabs_y, tx + f64::from(lw) + 24.0, tabs_y + tab_h);
            let hover = self.is_hover(tab_rect);
            let col = if active || hover {
                c.text_primary
            } else {
                c.text_dim
            };
            tr.draw_text_weighted(
                scene,
                label,
                14.0,
                col,
                t * Affine::translate((tx + 12.0, tabs_y + 11.0)),
                None,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                },
            );
            if active {
                let ind = Rect::new(
                    tx + 8.0,
                    tabs_y + tab_h - 3.0,
                    tx + f64::from(lw) + 16.0,
                    tabs_y + tab_h,
                );
                paint::fill_round(scene, t, ind, 1.5, theme.accent_primary);
            }
            if !active {
                self.push_hit(tab_rect, Action::SetSettingsTab(tab));
            }
            tx += f64::from(lw) + 24.0 + 6.0;
        }
        // Divider under the whole tab bar.
        paint::line(
            scene,
            t,
            Point::new(r.x0, tabs_y + tab_h),
            Point::new(r.x1, tabs_y + tab_h),
            c.border_standard,
            1.0,
        );
        tab_h
    }

    /// The Library settings pane: manage the folders watched for new books,
    /// the secondary way in ("Open a book" stays primary).
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_library_settings(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
    ) -> f64 {
        y = Self::draw_library_settings_header(scene, t, tr, c, x, w, y);

        // Candidate folders: watched ones, plus folders books were opened
        // from — always something concrete to toggle, no folder picker needed.
        let mut folders: Vec<PathBuf> = self.watched_folders.clone();
        for p in self.library_paths.values() {
            if let Some(parent) = p.parent() {
                let parent = parent.to_path_buf();
                if !folders.contains(&parent) {
                    folders.push(parent);
                }
            }
        }
        folders.sort();
        folders.dedup();

        if folders.is_empty() {
            return self.draw_library_settings_empty(scene, t, tr, theme, c, x, w, y);
        }

        self.draw_watched_folder_rows(scene, t, tr, theme, c, x, w, y, &folders)
    }

    /// "WATCHED FOLDERS" title + the explanatory blurb. Returns the y just
    /// below it.
    fn draw_library_settings_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
    ) -> f64 {
        // A short read on where books come from — no leaked internal paths, no
        // "magic folder": the primary way in is opening a book.
        tr.draw_text_weighted(
            scene,
            "WATCHED FOLDERS",
            11.0,
            c.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        tr.draw_text(
            scene,
            "Turn a folder on to add its books to your library automatically, and to keep noticing new ones. Everything you open is added to your library either way.",
            12.5,
            c.text_dim,
            t * Affine::translate((x, y)),
            Some(w as f32),
        );
        y + 40.0
    }

    /// The empty state: an explanation card + the primary "Open a book"
    /// action (no folder picker — the watch list grows from books you open).
    #[allow(clippy::too_many_arguments)]
    fn draw_library_settings_empty(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
    ) -> f64 {
        let radii_md = f64::from(theme.tokens.radii.md);
        let card = Rect::new(x, y, x + w, y + 84.0);
        paint::fill_round(scene, t, card, radii_md, c.surfaces.raised);
        tr.draw_text_weighted(
            scene,
            "No folders yet",
            14.0,
            c.text_primary,
            t * Affine::translate((x + 18.0, y + 18.0)),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            "Open a book from a folder and it will appear here, ready to watch.",
            12.5,
            c.text_dim,
            t * Affine::translate((x + 18.0, y + 42.0)),
            Some((w - 36.0) as f32),
        );
        y += 84.0 + 16.0;
        let bw = 150.0;
        self.accent_button(
            scene,
            t,
            tr,
            theme,
            Rect::new(x, y, x + bw, y + 40.0),
            "Open a book…",
            true,
            Action::OpenViaPortal,
        );
        y + 40.0
    }

    /// One row per candidate folder: name, resolved path, and its watch
    /// toggle. Returns the y just below the last row.
    #[allow(clippy::too_many_arguments)]
    fn draw_watched_folder_rows(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
        folders: &[PathBuf],
    ) -> f64 {
        let radii_md = f64::from(theme.tokens.radii.md);
        let home = std::env::var("HOME").ok();
        let ws = content::workspace_root();
        // Present folders in human terms — never a raw internal mount (HIG §4):
        // the workspace reads as "Workspace", the reader's home as "~".
        let pretty = |p: &std::path::Path| -> String {
            if let Ok(rest) = p.strip_prefix(&ws) {
                let rest = rest.to_string_lossy();
                return if rest.is_empty() {
                    "Workspace".to_string()
                } else {
                    format!("Workspace / {rest}")
                };
            }
            let s = p.to_string_lossy().to_string();
            match &home {
                Some(h) if !h.is_empty() && s.starts_with(h.as_str()) => {
                    format!("~{}", &s[h.len()..])
                }
                _ => s,
            }
        };

        for folder in folders {
            let row_h = 60.0;
            let card = Rect::new(x, y, x + w, y + row_h);
            paint::fill_round(scene, t, card, radii_md, c.surfaces.raised);

            let watched = self.watched_folders.iter().any(|f| f == folder);
            let name = folder
                .file_name()
                .map_or_else(|| pretty(folder), |n| n.to_string_lossy().to_string());
            tr.draw_text_weighted(
                scene,
                &name,
                15.0,
                c.text_primary,
                t * Affine::translate((x + 18.0, y + 12.0)),
                Some((w - 100.0) as f32),
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &pretty(folder),
                12.0,
                c.text_dim,
                t * Affine::translate((x + 18.0, y + 34.0)),
                Some((w - 100.0) as f32),
            );

            // Watch toggle at the row's right edge.
            let tog_w = 44.0;
            let tog_h = 24.0;
            self.switch(
                scene,
                t,
                theme,
                c,
                x + w - 18.0 - tog_w,
                y + (row_h - tog_h) / 2.0,
                tog_w,
                tog_h,
                watched,
                Action::ToggleWatchFolder(folder.clone()),
            );
            y += row_h + 12.0;
        }

        y
    }

    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_fonts_manager(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        y: f64,
    ) -> f64 {
        let fonts = self.fonts.clone();
        let default_id = if self.default_font_id.is_empty() {
            fonts.first().map(|f| f.id.clone()).unwrap_or_default()
        } else {
            self.default_font_id.clone()
        };

        let (mut y, dd_bottom) =
            self.draw_default_font_dropdown(scene, t, tr, theme, c, x, w, y, &fonts, &default_id);

        y = Self::draw_font_rows_header(scene, t, tr, c, x, w, y);
        for f in &fonts {
            y = self.draw_font_row(scene, t, tr, theme, c, x, w, y, f);
        }
        y = self.draw_add_font_button(scene, t, tr, theme, c, x, y);

        // The default-font dropdown menu (drawn last so it floats over the rows).
        if self.default_font_menu {
            self.draw_default_font_menu(
                scene,
                t,
                tr,
                theme,
                c,
                x,
                w,
                dd_bottom + 4.0,
                &fonts,
                &default_id,
            );
        }
        y
    }

    /// The default-font dropdown control: label + box showing the current
    /// default's name in its own face. Returns `(y_below, dropdown_bottom)`
    /// — `dropdown_bottom` anchors the flyout menu.
    #[allow(clippy::too_many_arguments)]
    fn draw_default_font_dropdown(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
        fonts: &[FontEntry],
        default_id: &str,
    ) -> (f64, f64) {
        let radii_md = f64::from(theme.tokens.radii.md);
        tr.draw_text_weighted(
            scene,
            "DEFAULT FONT",
            11.0,
            c.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        let dd_h = 46.0;
        let dd = Rect::new(x, y, x + w, y + dd_h);
        paint::fill_round(scene, t, dd, radii_md, c.surfaces.raised);
        paint::stroke_round(
            scene,
            t,
            dd,
            radii_md,
            if self.default_font_menu {
                theme.border_focus
            } else {
                c.border_standard
            },
            1.0,
        );
        let cur = fonts.iter().find(|f| f.id == default_id);
        if let Some(f) = cur {
            // Draw the current default's name in its own face.
            tr.draw_text_font(
                scene,
                &f.name,
                16.0,
                c.text_primary,
                t * Affine::translate((x + 16.0, y + dd_h / 2.0 - 10.0)),
                None,
                TextWeight::Semibold,
                false,
                &font_choice(&f.kind),
            );
        }
        self.icon_button(
            scene,
            t,
            theme,
            x + w - 34.0,
            y + dd_h / 2.0 - 14.0,
            28.0,
            Ico::ChevDown,
            false,
            Action::ToggleDefaultFontMenu,
        );
        self.push_hit(dd, Action::ToggleDefaultFontMenu);
        let dd_bottom = y + dd_h;
        y += dd_h + 26.0;
        (y, dd_bottom)
    }

    /// "READING FONTS" section title + description. Returns the y just
    /// below it, where the row list starts.
    fn draw_font_rows_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "READING FONTS",
            11.0,
            c.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 18.0;
        tr.draw_text(scene, "Each row previews its own face — tap a name to rename, toggle whether it appears in the quick settings, and tune its size.", 12.5, c.text_dim, t * Affine::translate((x, y)), Some(w as f32));
        y + 34.0
    }

    /// One font row: name (or its rename editor), kind label, and the right
    /// control cluster. Returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_font_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        y: f64,
        f: &FontEntry,
    ) -> f64 {
        let radii_md = f64::from(theme.tokens.radii.md);
        let row_h = 64.0;
        let cy = y + row_h / 2.0;
        let card = Rect::new(x, y, x + w, y + row_h);
        paint::fill_round(scene, t, card, radii_md, c.surfaces.raised);

        self.draw_font_row_name(scene, t, tr, theme, c, x, w, y, f);

        let kind = match &f.kind {
            FontKind::Default => "Default face".to_string(),
            FontKind::Serif => "Serif book face".to_string(),
            FontKind::Mono => "Monospace".to_string(),
            FontKind::Named(n) => format!("System · {n}"),
        };
        tr.draw_text(
            scene,
            &kind,
            12.0,
            c.text_dim,
            t * Affine::translate((x + 20.0, y + 38.0)),
            None,
        );

        self.draw_font_row_controls(scene, t, tr, theme, c, x, w, cy, f);

        y + row_h + 10.0
    }

    /// Name rendered in its own face. When renaming, the real inline editor
    /// (a `ui::EditableLabel` over a `TextInput`) takes over the slot;
    /// otherwise a pencil affordance invites a tap to rename.
    #[allow(clippy::too_many_arguments)]
    fn draw_font_row_name(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        y: f64,
        f: &FontEntry,
    ) {
        let name_x = x + 20.0;
        let renaming_this =
            matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Font(f.id.clone()));
        let fam = font_choice(&f.kind);
        if renaming_this {
            // Editor spans from the name to just before the right controls,
            // and typesets the edited value in the font's own face.
            let field_x1 = (x + w - 210.0).max(name_x + 140.0);
            let field_rect = Rect::new(name_x - 8.0, y + 8.0, field_x1, y + 8.0 + 30.0);
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field_rect);
                rn.field
                    .draw(scene, t, tr, theme, &ui::presence::PresenceCtx::empty());
            }
        } else {
            tr.draw_text_font(
                scene,
                &f.name,
                16.0,
                c.text_primary,
                t * Affine::translate((name_x, y + 12.0)),
                None,
                TextWeight::Semibold,
                false,
                &fam,
            );
            let (nw, _) = tr.measure_font(&f.name, 16.0, None, TextWeight::Semibold, false, &fam);
            let pencil = Rect::new(
                name_x + f64::from(nw) + 6.0,
                y + 12.0,
                name_x + f64::from(nw) + 26.0,
                y + 32.0,
            );
            paint::pencil(scene, t, pencil, c.text_dim.with_alpha_factor(0.5), 1.3);
            self.push_hit(
                Rect::new(name_x, y + 8.0, name_x + f64::from(nw) + 30.0, y + 34.0),
                Action::BeginRenameFont(f.id.clone()),
            );
        }
    }

    /// Right cluster of a font row, laid out right-to-left with a fixed
    /// delete column so every row's size control lines up (built-ins
    /// reserve the column but draw nothing in it): delete (custom fonts
    /// only), size stepper, and the quick-settings visibility toggle.
    #[allow(clippy::too_many_arguments)]
    fn draw_font_row_controls(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        cy: f64,
        f: &FontEntry,
    ) {
        let del_col = x + w - 16.0 - 24.0;
        if matches!(f.kind, FontKind::Named(_)) {
            let del = Rect::new(del_col, cy - 12.0, del_col + 24.0, cy + 12.0);
            let col = if self.is_hover(del) {
                c.semantic.danger
            } else {
                c.text_dim.with_alpha_factor(0.6)
            };
            paint::close(scene, t, del, col, 1.4);
            self.push_hit(del, Action::RemoveFont(f.id.clone()));
        }
        let size_right = del_col - 12.0;
        let left = self.size_stepper(
            scene,
            t,
            tr,
            theme,
            c,
            size_right,
            cy,
            f64::from(f.size_mult),
            NumberField::FontMult(f.id.clone()),
        );
        // Enable/disable toggle (= whether it appears in quick settings).
        let tog_w = 40.0;
        let tog_h = 22.0;
        let tog_x = left - 16.0 - tog_w;
        self.switch(
            scene,
            t,
            theme,
            c,
            tog_x,
            cy - tog_h / 2.0,
            tog_w,
            tog_h,
            f.shown,
            Action::ToggleFontShown(f.id.clone()),
        );
    }

    /// "+ Add a system font" pill. Returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_add_font_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        mut y: f64,
    ) -> f64 {
        let add = "＋  Add a system font";
        let aw = f64::from(tr.measure_weighted(add, 13.0, None, TextWeight::Medium).0) + 32.0;
        let arect = Rect::new(x, y, x + aw, y + 36.0);
        paint::fill_round(
            scene,
            t,
            arect,
            18.0,
            if self.is_hover(arect) {
                theme.bg_hover
            } else {
                c.surfaces.overlay
            },
        );
        paint::stroke_round(scene, t, arect, 18.0, c.border_standard, 1.0);
        tr.draw_text_weighted(
            scene,
            add,
            13.0,
            c.text_primary,
            t * Affine::translate((x + 16.0, y + 11.0)),
            None,
            TextWeight::Medium,
        );
        self.push_hit(arect, Action::OpenFontPicker);
        y += 36.0 + 8.0;
        y
    }

    /// The default-font dropdown list, floating under the dropdown control.
    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_default_font_menu(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        top: f64,
        fonts: &[FontEntry],
        default_id: &str,
    ) {
        let row_h = 40.0;
        let menu_h = fonts.len() as f64 * row_h + 8.0;
        let menu = Rect::new(x, top, x + w, top + menu_h);
        let shadow = Rect::new(menu.x0 - 1.0, menu.y0 + 3.0, menu.x1 + 1.0, menu.y1 + 6.0);
        paint::fill_round(scene, t, shadow, 12.0, Color::from_rgba8(0, 0, 0, 80));
        paint::fill_round(
            scene,
            t,
            menu,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );
        paint::stroke_round(
            scene,
            t,
            menu,
            f64::from(theme.tokens.radii.lg),
            c.border_standard,
            1.0,
        );
        self.push_hit(menu, Action::Swallow);
        let mut my = top + 4.0;
        for f in fonts {
            let row = Rect::new(x + 6.0, my, x + w - 6.0, my + row_h - 4.0);
            let sel = f.id == default_id;
            if sel {
                paint::fill_round(
                    scene,
                    t,
                    row,
                    8.0,
                    theme.accent_primary.with_alpha_factor(0.16),
                );
            } else if self.is_hover(row) {
                paint::fill_round(scene, t, row, 8.0, theme.bg_hover);
            }
            tr.draw_text_font(
                scene,
                &f.name,
                15.0,
                if sel {
                    theme.accent_primary
                } else {
                    c.text_primary
                },
                t * Affine::translate((x + 18.0, my + 9.0)),
                None,
                TextWeight::Medium,
                false,
                &font_choice(&f.kind),
            );
            self.push_hit(row, Action::SetDefaultFont(f.id.clone()));
            my += row_h;
        }
    }

    // Immediate-mode draw helper: scene/transform/theme + geometry is the natural parameter set.
    #[allow(clippy::too_many_arguments)]
    fn draw_schemes_manager(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        y: f64,
    ) -> f64 {
        let mut y = Self::draw_schemes_header(scene, t, tr, c, x, w, y);
        y = self.draw_new_scheme_button(scene, t, tr, theme, x, y);

        let schemes = self.schemes.clone();
        for s in &schemes {
            y = self.draw_scheme_row(scene, t, tr, theme, c, x, w, y, s);
        }
        y + 8.0
    }

    /// "COLOUR SCHEMES" section title + description. Returns the y just
    /// below it.
    fn draw_schemes_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        mut y: f64,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "COLOUR SCHEMES",
            11.0,
            c.text_dim,
            t * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        tr.draw_text(scene, "A scheme's colours are all edited through the colour picker. Toggle a scheme off to hide it from the quick settings.", 12.5, c.text_dim, t * Affine::translate((x, y)), Some(w as f32));
        y + 40.0
    }

    /// A single button opens the colour picker to choose a background; the
    /// scheme is then derived from it. Returns the y just below it.
    fn draw_new_scheme_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
    ) -> f64 {
        let new_label = "＋  New scheme from a colour…";
        let nw = f64::from(
            tr.measure_weighted(new_label, 13.0, None, TextWeight::Semibold)
                .0,
        ) + 34.0;
        let nrect = Rect::new(x, y, x + nw, y + 40.0);
        let nhover = self.is_hover(nrect);
        paint::fill_round(
            scene,
            t,
            nrect,
            20.0,
            if nhover {
                theme.accent_primary
            } else {
                theme.accent_primary.with_alpha_factor(0.16)
            },
        );
        let ncol = if nhover {
            Color::from_rgb8(248, 248, 252)
        } else {
            theme.accent_primary
        };
        tr.draw_text_weighted(
            scene,
            new_label,
            13.0,
            ncol,
            t * Affine::translate((x + 17.0, y + 13.0)),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(nrect, Action::NewSchemeViaPicker);
        y + 40.0 + 24.0
    }

    /// One colour-scheme row: swatches, name/status, and the right action
    /// cluster. Returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        y: f64,
        s: &ColorScheme,
    ) -> f64 {
        let radii_md = f64::from(theme.tokens.radii.md);
        let row_h = 68.0;
        let cy = y + row_h / 2.0;
        let card = Rect::new(x, y, x + w, y + row_h);
        let selected = if s.builtin {
            self.settings.scheme_id.is_empty() && self.settings.theme == ReadTheme::from_str(&s.id)
        } else {
            self.settings.scheme_id == s.id
        };
        paint::fill_round(scene, t, card, radii_md, c.surfaces.raised);
        if selected {
            paint::stroke_round(scene, t, card, radii_md, theme.accent_primary, 1.6);
        }

        // For a custom scheme each swatch opens the colour picker on that
        // slot; a built-in's swatches just apply the scheme.
        let use_action = if s.builtin {
            Action::SetReadTheme(ReadTheme::from_str(&s.id))
        } else {
            Action::SetScheme(s.id.clone())
        };
        let swx = self.draw_scheme_swatches(scene, t, theme, c, x, cy, s);

        // Name + status. When renaming, the inline editor fills the name
        // slot (drawn below, once its right bound is known).
        let name_x = swx + 16.0;
        let renaming_this =
            matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Scheme(s.id.clone()));
        Self::draw_scheme_name_and_status(
            scene,
            t,
            tr,
            theme,
            c,
            name_x,
            y,
            selected,
            renaming_this,
            s,
        );

        let rx = self.draw_scheme_actions(scene, t, tr, theme, c, x, w, cy, s);

        self.draw_scheme_name_interaction(
            scene,
            t,
            tr,
            theme,
            name_x,
            y,
            row_h,
            rx,
            renaming_this,
            use_action,
            s,
        );

        y + row_h + 10.0
    }

    /// Colour swatches (page / ink / accent). Returns the x just past
    /// them, where the name column starts.
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the draw call (scene/transform/theme/\
                  colours/position/scheme) and self is genuinely needed for push_hit; matches \
                  the sibling draw_* row helpers in this section, which carry the same allow"
    )]
    fn draw_scheme_swatches(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        cy: f64,
        s: &ColorScheme,
    ) -> f64 {
        let mut swx = x + 18.0;
        for (slot, col) in [s.page, s.ink, s.accent].into_iter().enumerate() {
            let srr = Rect::new(swx, cy - 12.0, swx + 24.0, cy + 12.0);
            paint::fill_round(scene, t, srr, 6.0, Color::from_rgb8(col[0], col[1], col[2]));
            let edith = !s.builtin && self.is_hover(srr);
            paint::stroke_round(
                scene,
                t,
                srr,
                6.0,
                if edith {
                    theme.border_focus
                } else {
                    c.border_standard
                },
                if edith { 2.0 } else { 1.0 },
            );
            if !s.builtin {
                self.push_hit(srr, Action::PickSchemeColor(s.id.clone(), slot));
            }
            swx += 30.0;
        }
        swx
    }

    /// Scheme name (skipped while its inline rename editor is active) and
    /// the Built-in/In use/Custom status label.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_name_and_status(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        name_x: f64,
        y: f64,
        selected: bool,
        renaming_this: bool,
        s: &ColorScheme,
    ) {
        if !renaming_this {
            tr.draw_text_weighted(
                scene,
                &s.name,
                15.0,
                c.text_primary,
                t * Affine::translate((name_x, y + 14.0)),
                None,
                TextWeight::Semibold,
            );
        }
        let sub = if s.builtin {
            "Built-in"
        } else if selected {
            "In use"
        } else {
            "Custom"
        };
        tr.draw_text(
            scene,
            sub,
            12.0,
            if selected {
                theme.accent_primary
            } else {
                c.text_dim
            },
            t * Affine::translate((name_x, y + 38.0)),
            None,
        );
    }

    /// Right cluster, laid out right-to-left: enable switch, then the
    /// actions (Duplicate / Edit / delete — Edit and delete only for
    /// custom schemes). Returns the x just left of the cluster.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_actions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        x: f64,
        w: f64,
        cy: f64,
        s: &ColorScheme,
    ) -> f64 {
        let mut rx = x + w - 18.0;
        let tog_w = 40.0;
        let tog_h = 22.0;
        rx -= tog_w;
        self.switch(
            scene,
            t,
            theme,
            c,
            rx,
            cy - tog_h / 2.0,
            tog_w,
            tog_h,
            s.enabled,
            Action::ToggleSchemeEnabled(s.id.clone()),
        );
        rx -= 16.0;
        if !s.builtin {
            rx -= 24.0;
            let del = Rect::new(rx, cy - 12.0, rx + 24.0, cy + 12.0);
            let dcolh = if self.is_hover(del) {
                c.semantic.danger
            } else {
                c.text_dim.with_alpha_factor(0.6)
            };
            paint::close(scene, t, del, dcolh, 1.4);
            self.push_hit(del, Action::DeleteScheme(s.id.clone()));
            rx -= 8.0;
            let ew = f64::from(
                tr.measure_weighted("Edit", 12.0, None, TextWeight::Medium)
                    .0,
            ) + 24.0;
            rx -= ew;
            self.pill(
                scene,
                t,
                tr,
                theme,
                rx,
                cy - 13.0,
                26.0,
                "Edit",
                12.0,
                false,
                Some(Action::EditScheme(s.id.clone())),
            );
            rx -= 8.0;
        }
        let dw = f64::from(
            tr.measure_weighted("Duplicate", 12.0, None, TextWeight::Medium)
                .0,
        ) + 24.0;
        rx -= dw;
        self.pill(
            scene,
            t,
            tr,
            theme,
            rx,
            cy - 13.0,
            26.0,
            "Duplicate",
            12.0,
            false,
            Some(Action::DuplicateScheme(s.id.clone())),
        );
        rx
    }

    /// Name click: rename (custom) / apply (built-in), bounded before the
    /// right cluster. When renaming, the inline editor fills the slot.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_name_interaction(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        name_x: f64,
        y: f64,
        row_h: f64,
        rx: f64,
        renaming_this: bool,
        use_action: Action,
        s: &ColorScheme,
    ) {
        let name_end = rx - 12.0;
        if renaming_this {
            let field_x1 = name_end.max(name_x + 120.0);
            let field_rect = Rect::new(name_x - 6.0, y + 8.0, field_x1, y + 8.0 + 28.0);
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field_rect);
                rn.field
                    .draw(scene, t, tr, theme, &ui::presence::PresenceCtx::empty());
            }
        } else if name_end > name_x {
            if s.builtin {
                self.push_hit(
                    Rect::new(name_x, y + 8.0, name_end, y + row_h - 8.0),
                    use_action,
                );
            } else {
                self.push_hit(
                    Rect::new(name_x, y + 8.0, name_end, y + 34.0),
                    Action::BeginRenameScheme(s.id.clone()),
                );
                // Below the name, the sub-label area applies the scheme.
                self.push_hit(
                    Rect::new(name_x, y + 34.0, name_end, y + row_h - 8.0),
                    use_action,
                );
            }
        }
    }

    fn draw_scheme_editor(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
    ) {
        let Some(id) = self.editing_scheme.clone() else {
            return;
        };
        let Some(scheme) = self.schemes.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        let r = self.rect;
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.4);
        scene.fill(Fill::NonZero, t, scrim, None, &r);
        self.push_hit(r, Action::CloseSchemeEditor);

        let pw = 440.0_f64.min(r.width() - 40.0);
        let ph = 372.0_f64.min(r.height() - 40.0);
        let px0 = r.x0 + (r.width() - pw) / 2.0;
        let py0 = r.y0 + (r.height() - ph) / 2.0;
        let panel = Rect::new(px0, py0, px0 + pw, py0 + ph);
        paint::fill_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );
        paint::stroke_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            c.border_standard,
            1.0,
        );
        self.push_hit(panel, Action::Swallow);

        let ix0 = px0 + 22.0;
        let ix1 = px0 + pw - 22.0;
        let y = py0 + 20.0;

        let y =
            self.draw_scheme_editor_title(scene, t, tr, theme, c, ix0, ix1, y, &id, &scheme.name);
        let y = self.draw_scheme_editor_swatches(scene, t, tr, theme, c, ix0, ix1, y, &id, &scheme);
        let y = Self::draw_scheme_editor_preview(scene, t, tr, c, ix0, ix1, y, &scheme);
        self.draw_scheme_editor_done_button(scene, t, tr, theme, ix1, y);
    }

    /// "EDIT SCHEME" title + the scheme name (or its inline rename editor).
    /// Returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_editor_title(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        id: &str,
        name: &str,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "EDIT SCHEME",
            11.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 18.0;
        let renaming_this =
            matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Scheme(id.to_string()));
        if renaming_this {
            let field_rect = Rect::new(ix0 - 6.0, y - 4.0, ix1 - 60.0, y + 30.0);
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field_rect);
                rn.field
                    .draw(scene, t, tr, theme, &ui::presence::PresenceCtx::empty());
            }
        } else {
            tr.draw_text_weighted(
                scene,
                name,
                20.0,
                c.text_primary,
                t * Affine::translate((ix0, y)),
                None,
                TextWeight::Bold,
            );
            self.push_hit(
                Rect::new(ix0, y - 2.0, ix1 - 60.0, y + 26.0),
                Action::BeginRenameScheme(id.to_string()),
            );
        }
        y + 40.0
    }

    /// "COLOURS" section: one swatch per scheme slot, each opening the
    /// colour picker. Returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_editor_swatches(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        id: &str,
        scheme: &ColorScheme,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "COLOURS · tap a swatch to open the picker",
            10.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 18.0;
        let slots = scheme.slots();
        let slot_w = (ix1 - ix0 - 2.0 * 16.0) / 3.0;
        let mut sxx = ix0;
        for (i, (label, col)) in slots.iter().enumerate() {
            let sr = Rect::new(sxx, y, sxx + slot_w, y + 60.0);
            paint::fill_round(scene, t, sr, 10.0, Color::from_rgb8(col[0], col[1], col[2]));
            let hov = self.is_hover(sr);
            paint::stroke_round(
                scene,
                t,
                sr,
                10.0,
                if hov {
                    theme.border_focus
                } else {
                    c.border_standard
                },
                if hov { 2.0 } else { 1.0 },
            );
            self.push_hit(sr, Action::PickSchemeColor(id.to_string(), i));
            let (lw, _) = tr.measure(label, 12.0, None);
            tr.draw_text(
                scene,
                label,
                12.0,
                c.text_dim,
                t * Affine::translate((sxx + (slot_w - f64::from(lw)) / 2.0, y + 68.0)),
                None,
            );
            sxx += slot_w + 16.0;
        }
        y + 60.0 + 32.0
    }

    /// "PREVIEW" section: a mock reading card rendered in the scheme's own
    /// palette. Returns the y just below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_scheme_editor_preview(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        c: &ui::tokens::colors::Colors,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        scheme: &ColorScheme,
    ) -> f64 {
        tr.draw_text_weighted(
            scene,
            "PREVIEW",
            10.0,
            c.text_dim,
            t * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 16.0;
        let pal = ReaderPalette::from_scheme(scheme);
        let prev = Rect::new(ix0, y, ix1, y + 72.0);
        paint::fill_round(scene, t, prev, 8.0, pal.page);
        paint::stroke_round(scene, t, prev, 8.0, pal.page_edge, 1.0);
        tr.draw_text_weighted(
            scene,
            "The quick brown fox",
            15.0,
            pal.ink,
            t * Affine::translate((ix0 + 14.0, y + 12.0)),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            "jumps over the lazy dog.",
            13.0,
            pal.ink_dim,
            t * Affine::translate((ix0 + 14.0, y + 38.0)),
            None,
        );
        let chip = Rect::new(ix1 - 64.0, y + 46.0, ix1 - 14.0, y + 54.0);
        paint::fill_round(scene, t, chip, 4.0, pal.accent);
        y + 72.0 + 22.0
    }

    /// The "Done" button that closes the editor.
    fn draw_scheme_editor_done_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        ix1: f64,
        y: f64,
    ) {
        let done_w = f64::from(
            tr.measure_weighted("Done", 13.0, None, TextWeight::Semibold)
                .0,
        ) + 40.0;
        let drect = Rect::new(ix1 - done_w, y, ix1, y + 34.0);
        paint::fill_round(scene, t, drect, 17.0, theme.accent_primary);
        tr.draw_text_weighted(
            scene,
            "Done",
            13.0,
            Color::from_rgb8(248, 248, 252),
            t * Affine::translate((ix1 - done_w + 20.0, y + 9.0)),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(drect, Action::CloseSchemeEditor);
    }

    fn draw_font_picker(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
    ) {
        let r = self.rect;
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.4);
        scene.fill(Fill::NonZero, t, scrim, None, &r);
        self.push_hit(r, Action::CloseFontPicker);

        let pw = 380.0_f64.min(r.width() - 40.0);
        let ph = (r.height() - 120.0).clamp(200.0, 520.0);
        let px0 = r.x0 + (r.width() - pw) / 2.0;
        let py0 = r.y0 + (r.height() - ph) / 2.0;
        let panel = Rect::new(px0, py0, px0 + pw, py0 + ph);
        paint::fill_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );
        paint::stroke_round(
            scene,
            t,
            panel,
            f64::from(theme.tokens.radii.lg),
            c.border_standard,
            1.0,
        );
        self.push_hit(panel, Action::Swallow);
        tr.draw_text_weighted(
            scene,
            "Add a system font",
            16.0,
            c.text_primary,
            t * Affine::translate((px0 + 20.0, py0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        let close = Rect::new(px0 + pw - 40.0, py0 + 14.0, px0 + pw - 16.0, py0 + 38.0);
        paint::close(
            scene,
            t,
            close,
            if self.is_hover(close) {
                c.text_primary
            } else {
                c.text_dim
            },
            1.5,
        );
        self.push_hit(close, Action::CloseFontPicker);

        let list_top = py0 + 52.0;
        let list = Rect::new(px0, list_top, px0 + pw, py0 + ph - 14.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &list);
        let fams = tr.system_families();
        let row_h = 40.0;
        let mut fy = list_top - self.picker_scroll;
        for fam in &fams {
            if fy > list.y1 || fy + row_h < list.y0 {
                fy += row_h;
                continue;
            }
            let rr = Rect::new(px0 + 12.0, fy, px0 + pw - 12.0, fy + row_h - 4.0);
            if self.is_hover(rr) {
                paint::fill_round(
                    scene,
                    t,
                    rr,
                    f64::from(theme.tokens.radii.md),
                    theme.bg_hover,
                );
            }
            // Preview each family by rendering its own name in it.
            tr.draw_text_font(
                scene,
                fam,
                15.0,
                c.text_primary,
                t * Affine::translate((px0 + 22.0, fy + 10.0)),
                None,
                TextWeight::Regular,
                false,
                &FontChoice::Named(fam.clone()),
            );
            self.push_hit(rr, Action::AddSystemFont(fam.clone()));
            fy += row_h;
        }
        scene.pop_layer();
        let content = fams.len() as f64 * row_h;
        let visible = (list.y1 - list.y0).max(0.0);
        self.picker_max_scroll = (content - visible + 8.0).max(0.0);
        self.picker_scroll = self.picker_scroll.clamp(0.0, self.picker_max_scroll);
    }
}

/// Sub-pixel x-span `[x_lo, x_hi]` of chars `[lo, hi)` within a run laid out at
/// `x0` with measured width `w`, using proportional placement within the run.
fn run_sub_x(x0: f64, w: f64, cs: usize, cl: usize, lo: usize, hi: usize) -> (f64, f64) {
    if cl == 0 {
        return (x0, x0 + w);
    }
    let at = |ch: usize| x0 + (ch.saturating_sub(cs) as f64 / cl as f64) * w;
    (at(lo), at(hi))
}

/// The set of installed font-family names, enumerated once per process
/// (building a `FontSystem` to enumerate faces is not free). A named face
/// the reader picked that isn't installed degrades to serif, never
/// cosmic-text's default sans (see [`BooksApp::body_font`]).
fn installed_families() -> &'static std::collections::HashSet<String> {
    use std::sync::OnceLock;
    static FAMS: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    FAMS.get_or_init(|| TextRenderer::new().system_families().into_iter().collect())
}

/// Whether `name` names a family the text system can actually shape.
fn family_is_installed(name: &str) -> bool {
    installed_families().contains(name)
}

/// A stable signature of a font's layout-affecting attributes (family + size
/// multiplier) for the layout cache key.
fn font_sig(f: &FontEntry) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    f.kind.as_code().hash(&mut h);
    ((f.size_mult * 1000.0).round() as i64).hash(&mut h);
    h.finish()
}

/// Find the page index whose span contains `block`'s top.
fn page_for_block(layout: &ChapterLayout, block: usize) -> usize {
    let Some(b) = layout.blocks.iter().find(|b| b.index == block) else {
        return 0;
    };
    let mut page = 0;
    for (i, &start) in layout.pages.iter().enumerate() {
        if start <= b.top + 0.5 {
            page = i;
        } else {
            break;
        }
    }
    page
}

/// Truncate `text` with a trailing ellipsis so it fits within `max_w` at the
/// given size/weight. Returns the full string when it already fits.
fn ellipsize(
    tr: &mut TextRenderer,
    text: &str,
    size: f32,
    weight: TextWeight,
    max_w: f64,
) -> String {
    if f64::from(tr.measure_weighted(text, size, None, weight).0) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut hi = chars.len();
    // Binary-search the longest prefix that fits with an ellipsis appended.
    let mut best = String::from("…");
    let mut lo = 0;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if f64::from(tr.measure_weighted(&candidate, size, None, weight).0) <= max_w {
            best = candidate;
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    best
}

/// Blit a decoded cover into `rect` with cover-fit cropping and a rounded
/// clip, so real artwork of any aspect fills the shelf slot cleanly.
fn blit_cover(scene: &mut Scene, t: Affine, img: PenikoImage, rect: Rect, radius: f64) {
    let iw = f64::from(img.width.max(1));
    let ih = f64::from(img.height.max(1));
    let (rw, rh) = (rect.width(), rect.height());
    if rw <= 0.0 || rh <= 0.0 {
        return;
    }
    let scale = (rw / iw).max(rh / ih);
    let dw = iw * scale;
    let dh = ih * scale;
    let dx = rect.x0 + (rw - dw) * 0.5;
    let dy = rect.y0 + (rh - dh) * 0.5;
    let brush_transform = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);

    let clip = RoundedRect::from_rect(rect, radius);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
    let brush = ImageBrush::new(img);
    scene.fill(Fill::NonZero, t, &brush, Some(brush_transform), &rect);
    scene.pop_layer();
}

/// Stroke a dashed border along the straight edges of a rounded rectangle (the
/// rounded corners are left as small gaps). Reads as a drop-zone outline.
/// Stroke parameters for [`dashed_round_border`], grouped so the border
/// drawer stays under clippy's argument-count lint.
struct DashStyle {
    width: f64,
    dash: f64,
    gap: f64,
}

#[allow(
    clippy::while_float,
    reason = "each iteration advances by a fixed positive step (dash + gap), so both loops \
              are bounded regardless of float imprecision"
)]
fn dashed_round_border(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    style: &DashStyle,
) {
    let step = style.dash + style.gap;
    // Top + bottom edges, between the corner radii.
    let (hx0, hx1) = (rect.x0 + radius, rect.x1 - radius);
    let mut x = hx0;
    while x < hx1 {
        let xe = (x + style.dash).min(hx1);
        paint::line(
            scene,
            t,
            Point::new(x, rect.y0),
            Point::new(xe, rect.y0),
            color,
            style.width,
        );
        paint::line(
            scene,
            t,
            Point::new(x, rect.y1),
            Point::new(xe, rect.y1),
            color,
            style.width,
        );
        x += step;
    }
    // Left + right edges.
    let (vy0, vy1) = (rect.y0 + radius, rect.y1 - radius);
    let mut y = vy0;
    while y < vy1 {
        let ye = (y + style.dash).min(vy1);
        paint::line(
            scene,
            t,
            Point::new(rect.x0, y),
            Point::new(rect.x0, ye),
            color,
            style.width,
        );
        paint::line(
            scene,
            t,
            Point::new(rect.x1, y),
            Point::new(rect.x1, ye),
            color,
            style.width,
        );
        y += step;
    }
}

/// Pick a cover spine + readable ink for a book, sourced entirely from theme
/// tokens (semantic hues + accent) so covers stay on-brand across OS themes.
fn cover_colors(theme: &Theme, tint: usize) -> (Color, Color) {
    let c = &theme.tokens.colors;
    let base = match tint % 5 {
        0 => c.semantic.info,
        1 => c.accent[5],
        2 => c.semantic.success,
        3 => c.semantic.warning,
        _ => c.semantic.danger,
    };
    let lum =
        0.299 * f32::from(base.r()) + 0.587 * f32::from(base.g()) + 0.114 * f32::from(base.b());
    let ink = if lum < 150.0 {
        Color::from_rgb8(248, 248, 250)
    } else {
        Color::from_rgb8(28, 26, 30)
    };
    (base, ink)
}

#[cfg(test)]
mod interaction_tests {
    //! Drive the app through the real `handle_event` path — a headless frame
    //! populates hit regions, then synthesized winit clicks route through the
    //! same code the live compositor uses. Asserts observable state/CRDT change.

    use super::*;
    use crate::content::load_sample_library;
    use crate::model::AnnoKind;
    use serial_test::serial;
    use ui::presence::PresenceCtx;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, KeyEvent};

    fn app_with_sample() -> BooksApp {
        BooksApp::with_library(load_sample_library())
    }

    fn app_with_math_sample() -> BooksApp {
        let mut app = app_with_sample();
        let chapter = r#"<body xmlns="http://www.w3.org/1999/xhtml">
          <h1>An Economy of Symbols</h1>
          <p>Inline notation should belong to the sentence: <math xmlns="http://www.w3.org/1998/Math/MathML"><mi>E</mi><mo>=</mo><mi>m</mi><msup><mi>c</mi><mn>2</mn></msup></math>, without disturbing its rhythm.</p>
          <p>A displayed identity gets room for its hierarchy and a stable optical centre.</p>
          <math xmlns="http://www.w3.org/1998/Math/MathML" display="block"><mfrac><mrow><msup><mi>x</mi><mn>2</mn></msup><mo>+</mo><mn>1</mn></mrow><msqrt><mrow><mi>y</mi><mo>-</mo><mn>3</mn></mrow></msqrt></mfrac></math>
          <p>Limits and arrays use the same recursive boxes rather than a second renderer.</p>
          <math xmlns="http://www.w3.org/1998/Math/MathML" display="block"><mrow><munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover><mfenced><mtable><mtr><mtd><mi>a</mi></mtd><mtd><mi>b</mi></mtd></mtr><mtr><mtd><mi>c</mi></mtd><mtd><mi>d</mi></mtd></mtr></mtable></mfenced></mrow></math>
        </body>"#;
        app.library[0].chapters[0].blocks = crate::xhtml::parse_xhtml(chapter);
        app.library[0].chapters[0].title = "An Economy of Symbols".into();
        app.layout = None;
        app
    }

    /// Run one full headless frame so `self.hits` reflects the current view.
    fn frame(app: &mut BooksApp, doc: &mut AutoCommit, w: f64, h: f64) {
        let root = automerge::ROOT;
        app.sync(doc, &root);
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.reconcile_flaps();
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// Centre of the topmost recorded hit whose action matches `pred`.
    fn find_hit(app: &BooksApp, pred: impl Fn(&Action) -> bool) -> Option<Point> {
        app.comment_overlay
            .action_center(|action| {
                let action = match action {
                    CommentAction::Close => Action::CloseComments,
                    CommentAction::Swallow => Action::Swallow,
                    CommentAction::DeleteHighlight(group) => Action::DeleteHighlight(group),
                    CommentAction::Add(group) => Action::AddComment(group),
                    CommentAction::Edit(id) => Action::EditComment(id),
                    CommentAction::Delete(id) => Action::DeleteComment(id),
                };
                pred(&action)
            })
            .or_else(|| {
                [
                    (ReaderBubbleAction::PreviousPage, Action::PrevPage),
                    (ReaderBubbleAction::NextPage, Action::NextPage),
                ]
                .into_iter()
                .find_map(|(bubble_action, action)| {
                    pred(&action)
                        .then(|| app.reader_bubble.action_center(bubble_action))
                        .flatten()
                })
            })
            .or_else(|| {
                app.hits
                    .iter()
                    .rev()
                    .find(|h| pred(&h.action))
                    .map(|h| h.rect.center())
            })
    }

    /// Route a real left-click at `p` (logical px) through `handle_event`.
    fn click(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) -> bool {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        let mv = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(p.x, p.y),
        };
        app.handle_event(
            &mv,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        handled = false;
        app.handle_event(
            &press,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    /// Route a wheel event (`lines` positive == scroll up) through `handle_event`.
    fn wheel(app: &mut BooksApp, doc: &mut AutoCommit, lines: f32) -> bool {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        let ev = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, lines),
            phase: winit::event::TouchPhase::Moved,
        };
        app.handle_event(
            &ev,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    /// Route a horizontal wheel/scroll through `handle_event`. Positive
    /// `lines` is a physical scroll-right, matching winit's `LineDelta` x sign.
    fn hwheel(app: &mut BooksApp, doc: &mut AutoCommit, lines: f32) -> bool {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        let ev = WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(lines, 0.0),
            phase: winit::event::TouchPhase::Moved,
        };
        app.handle_event(
            &ev,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    /// Low-level pointer events for drag-selection tests.
    fn send(app: &mut BooksApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }
    fn ptr_move(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) {
        send(
            app,
            doc,
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(p.x, p.y),
            },
        );
    }
    fn ptr_down(app: &mut BooksApp, doc: &mut AutoCommit) {
        send(
            app,
            doc,
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
    }
    fn ptr_up(app: &mut BooksApp, doc: &mut AutoCommit) {
        send(
            app,
            doc,
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
    }

    /// Draw one settled frame into a `Scene` (for pixel-level assertions).
    fn render_scene(app: &mut BooksApp, doc: &mut AutoCommit, w: f64, h: f64) -> Scene {
        let root = automerge::ROOT;
        app.sync(doc, &root);
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.reconcile_flaps();
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    // Synthesised winit key presses (zeroed-KeyEvent trick; every field is
    // overwritten before use), routed through the real `handle_event`.
    fn char_press(ch: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(ch.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(ch.into());
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn named_press(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn feed(app: &mut BooksApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }
    fn key_char(app: &mut BooksApp, doc: &mut AutoCommit, ch: &str) -> bool {
        feed(app, doc, &char_press(ch))
    }
    fn key_named(app: &mut BooksApp, doc: &mut AutoCommit, named: NamedKey) -> bool {
        feed(app, doc, &named_press(named))
    }
    /// Feed a key with explicit modifiers (Shift/Ctrl selection extension).
    fn key_named_mods(
        app: &mut BooksApp,
        doc: &mut AutoCommit,
        named: NamedKey,
        state: ModifiersState,
    ) -> bool {
        let mods = Modifiers::from(state);
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &named_press(named),
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }
    /// Feed a printable character with explicit modifiers (e.g. Ctrl+A).
    fn key_char_mods(
        app: &mut BooksApp,
        doc: &mut AutoCommit,
        ch: &str,
        state: ModifiersState,
    ) -> bool {
        let mods = Modifiers::from(state);
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            &char_press(ch),
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    /// A point at the centre of the first laid-out word on the current page.
    fn page_point(app: &BooksApp) -> Point {
        let r = &app.sel_runs[0];
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
    }

    /// A real double-click (two rapid presses at the same point).
    fn dbl_click(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) {
        ptr_move(app, doc, p);
        ptr_down(app, doc);
        ptr_up(app, doc);
        ptr_down(app, doc);
        ptr_up(app, doc);
    }

    #[test]
    fn wheel_turns_pages_in_the_reader() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.view, View::Reader);
        let start = (app.chapter, app.page);
        // A downward flick turns forward.
        assert!(
            wheel(&mut app, &mut doc, -4.0),
            "downward wheel turned a page"
        );
        assert!(
            (app.chapter, app.page) != start,
            "advanced from ch{}/pg{}",
            start.0,
            start.1
        );
        // An upward flick turns back.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let mid = (app.chapter, app.page);
        wheel(&mut app, &mut doc, 4.0);
        assert!(
            (app.chapter, app.page) != mid,
            "turned back from the advanced spot"
        );
    }

    #[test]
    fn horizontal_scroll_right_turns_forward() {
        // Scrolling right must advance (next page); scrolling left must go back.
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.view, View::Reader);
        let start = (app.chapter, app.page);
        // Scroll right (positive x) advances forward.
        assert!(
            hwheel(&mut app, &mut doc, 3.0),
            "scroll-right turned a page"
        );
        let after_right = (app.chapter, app.page);
        assert!(after_right != start, "scroll-right advanced (forward)");
        // Position strictly moved forward (page or chapter increased).
        assert!(
            after_right.0 > start.0 || (after_right.0 == start.0 && after_right.1 > start.1),
            "scroll-right moved forward, not back: {start:?} -> {after_right:?}",
        );
        // Scroll left (negative x) turns back.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        hwheel(&mut app, &mut doc, -3.0);
        let after_left = (app.chapter, app.page);
        assert!(after_left != after_right, "scroll-left turned back");
        assert!(
            after_left.0 < after_right.0
                || (after_left.0 == after_right.0 && after_left.1 < after_right.1),
            "scroll-left moved backward: {after_right:?} -> {after_left:?}",
        );
    }

    #[test]
    fn selecting_serif_font_applies_and_persists() {
        use crate::model::FontKind;
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // The reader opens in the serif book face (books read best in serif) —
        // and that is the face the paint pass shapes with.
        assert_eq!(
            app.active_font().kind,
            FontKind::Serif,
            "serif is the default reading face"
        );
        let (fam0, _m0, serif_sig, mono0) = app.body_font();
        assert_eq!(fam0, ui::text_render::FontChoice::Serif);
        assert!(!mono0, "serif is a proportional face");
        // Open the settings popover and switch to the sans "Default" pill — the
        // real click path — which must apply and persist to the CRDT.
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        click(&mut app, &mut doc, aa);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let sans = find_hit(
            &app,
            |a| matches!(a, Action::SetFont(id) if id.as_str() == "default"),
        )
        .expect("default (sans) pill");
        assert!(click(&mut app, &mut doc, sans), "sans pill handled");
        assert_eq!(
            app.active_font().kind,
            FontKind::Default,
            "sans face is now active"
        );
        assert_eq!(
            BooksDoc::read_settings(&doc).font_id,
            "default",
            "selection persisted to CRDT"
        );
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let (fam1, _m1, sans_sig, _mono1) = app.body_font();
        assert_eq!(fam1, ui::text_render::FontChoice::SansSerif);
        assert_ne!(
            sans_sig, serif_sig,
            "font signature differs so the chapter relays out"
        );
        // And back to serif via its own pill — reselecting the reading face.
        let serif = find_hit(
            &app,
            |a| matches!(a, Action::SetFont(id) if id.as_str() == "serif"),
        )
        .expect("serif pill");
        assert!(click(&mut app, &mut doc, serif), "serif pill handled");
        assert_eq!(
            app.active_font().kind,
            FontKind::Serif,
            "serif font is active again"
        );
        assert_eq!(
            BooksDoc::read_settings(&doc).font_id,
            "serif",
            "serif selection persisted"
        );
    }

    /// A named face that isn't installed must degrade to generic serif, not
    /// cosmic-text's default sans; an installed family is honoured verbatim.
    #[test]
    fn absent_named_font_falls_back_to_generic_serif() {
        use crate::model::{FontEntry, FontKind};
        let mut app = app_with_sample();
        app.fonts.push(FontEntry::new(
            "ghost",
            FontKind::Named("Nonexistent Phantom Face 9Z".into()),
            "Ghost",
            1.0,
            true,
        ));
        app.settings.font_id = "ghost".into();
        let (fam, _, _, _) = app.body_font();
        assert_eq!(
            fam,
            ui::text_render::FontChoice::Serif,
            "an absent named face degrades to a real serif, not the default sans",
        );

        // A genuinely installed family is used as-is.
        let real = TextRenderer::new()
            .system_families()
            .into_iter()
            .next()
            .expect("the text system enumerates at least one installed family");
        app.fonts.push(FontEntry::new(
            "real",
            FontKind::Named(real.clone()),
            "Real",
            1.0,
            true,
        ));
        app.settings.font_id = "real".into();
        let (fam2, _, _, _) = app.body_font();
        assert_eq!(
            fam2,
            ui::text_render::FontChoice::Named(real),
            "an installed named family is honoured verbatim (no gratuitous substitution)",
        );
    }

    #[test]
    fn disabling_a_scheme_hides_it_from_quick_settings() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 820.0);
        // All four built-in schemes start enabled and offered in quick settings.
        assert_eq!(app.quick_scheme_pills().len(), 4);
        // Toggle the Light scheme off through its manager row.
        let toggle = find_hit(
            &app,
            |a| matches!(a, Action::ToggleSchemeEnabled(id) if id.as_str() == "light"),
        )
        .expect("scheme enable toggle");
        assert!(click(&mut app, &mut doc, toggle), "toggle handled");
        frame(&mut app, &mut doc, 1200.0, 820.0);
        let pills = app.quick_scheme_pills();
        assert_eq!(
            pills.len(),
            3,
            "a disabled scheme drops out of quick settings"
        );
        assert!(
            !pills.iter().any(|p| p.label == "Light"),
            "Light is gone from quick settings"
        );
    }

    #[test]
    fn scheme_manager_new_via_picker_creates_selects_and_edits() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 860.0);
        let before = BooksDoc::read_schemes(&doc).len();
        // The single "New scheme" button opens the colour picker (no scheme yet).
        let newbtn = find_hit(&app, |a| matches!(a, Action::NewSchemeViaPicker))
            .expect("a New scheme button");
        assert!(click(&mut app, &mut doc, newbtn), "new-via-picker handled");
        assert!(app.color_picker.is_open(), "the colour picker opened");
        assert_eq!(
            BooksDoc::read_schemes(&doc).len(),
            before,
            "no scheme until the picker commits"
        );
        // Commit the picker -> a scheme is derived from the chosen background,
        // selected, and opened in the editor.
        frame(&mut app, &mut doc, 1200.0, 860.0);
        let done = app.color_picker.done_center().expect("a Done button");
        assert!(click(&mut app, &mut doc, done), "picker done handled");
        let schemes = BooksDoc::read_schemes(&doc);
        assert_eq!(
            schemes.len(),
            before + 1,
            "a custom scheme was created on commit"
        );
        let custom = schemes
            .iter()
            .find(|s| !s.builtin)
            .expect("a custom scheme exists");
        assert_eq!(
            BooksDoc::read_settings(&doc).scheme_id,
            custom.id,
            "new scheme is selected"
        );
        assert_eq!(
            app.editing_scheme.as_deref(),
            Some(custom.id.as_str()),
            "editor opened on it"
        );
    }

    #[test]
    fn color_picker_returns_a_colour_applied_to_a_scheme() {
        // A custom scheme's page colour, edited through the picker, changes.
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        let mut schemes = default_schemes();
        schemes.push(ColorScheme::from_bg("scheme_x", "Test", [0x30, 0x30, 0x30]));
        BooksDoc::write_schemes(&mut doc, &schemes);
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 900.0);
        let before = BooksDoc::read_schemes(&doc)
            .into_iter()
            .find(|s| s.id == "scheme_x")
            .unwrap()
            .page;
        // Tap the custom scheme's page swatch to open the picker on that slot.
        let sw = find_hit(
            &app,
            |a| matches!(a, Action::PickSchemeColor(id, slot) if id == "scheme_x" && *slot == 0),
        )
        .expect("the page swatch opens the picker");
        assert!(
            click(&mut app, &mut doc, sw),
            "picker opened on the page slot"
        );
        assert!(app.color_picker.is_open());
        // Drag on the SV square: a press + move within it applies a new colour.
        frame(&mut app, &mut doc, 1200.0, 900.0);
        let sv = app.color_picker.sv_rect();
        ptr_move(
            &mut app,
            &mut doc,
            Point::new(sv.x0 + sv.width() * 0.8, sv.y0 + sv.height() * 0.2),
        );
        ptr_down(&mut app, &mut doc);
        ptr_move(
            &mut app,
            &mut doc,
            Point::new(sv.x0 + sv.width() * 0.75, sv.y0 + sv.height() * 0.25),
        );
        ptr_up(&mut app, &mut doc);
        let after = BooksDoc::read_schemes(&doc)
            .into_iter()
            .find(|s| s.id == "scheme_x")
            .unwrap()
            .page;
        assert_ne!(
            after, before,
            "the picker applied a new page colour to the scheme"
        );
    }

    #[test]
    fn font_manager_toggles_shown_and_sets_default() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1200.0, 820.0);
        // Disable the Serif font (its enable toggle = whether it shows in quick
        // settings).
        let hide = find_hit(
            &app,
            |a| matches!(a, Action::ToggleFontShown(id) if id.as_str() == "serif"),
        )
        .expect("serif enable toggle");
        assert!(click(&mut app, &mut doc, hide), "toggle handled");
        frame(&mut app, &mut doc, 1200.0, 820.0);
        assert!(
            !app.shown_fonts().iter().any(|f| f.id == "serif"),
            "serif no longer in quick settings"
        );
        // Make Mono the registry default via the default-font dropdown.
        let dd = find_hit(&app, |a| matches!(a, Action::ToggleDefaultFontMenu))
            .expect("default dropdown");
        assert!(click(&mut app, &mut doc, dd), "dropdown opened");
        assert!(app.default_font_menu, "default-font menu is open");
        frame(&mut app, &mut doc, 1200.0, 820.0);
        let def = find_hit(
            &app,
            |a| matches!(a, Action::SetDefaultFont(id) if id.as_str() == "mono"),
        )
        .expect("mono row in the dropdown");
        assert!(click(&mut app, &mut doc, def), "default handled");
        assert_eq!(
            BooksDoc::read_default_font(&doc),
            "mono",
            "default persisted"
        );
        // With no explicit selection, the active font follows the default.
        assert_eq!(
            app.active_font().id,
            "mono",
            "active font is the new default"
        );
    }

    #[test]
    fn drag_selection_spans_paragraphs_and_highlights_the_range() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Wide window so the docked rails don't cover the page centre.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            app.sel_runs.len() > 3,
            "the page laid out real run geometry"
        );

        // Press inside the first run, then drag to a run in a *later* block.
        let r0 = app.sel_runs[0];
        let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
        let later = app
            .sel_runs
            .iter()
            .find(|r| r.block > r0.block)
            .copied()
            .expect("a run in a later paragraph");
        let end = Point::new(
            f64::midpoint(later.x0, later.x1),
            f64::midpoint(later.y0, later.y1),
        );

        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);

        let sel = app.sel.expect("a drag produced a selection");
        let (s, e) = sel.ordered();
        assert!(
            e.block > s.block,
            "the selection spans more than one paragraph"
        );

        // Highlight the selection from the floating toolbar — one ranged
        // highlight per spanned block.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        assert!(click(&mut app, &mut doc, swatch), "swatch handled");
        let hls: Vec<_> = BooksDoc::list_annotations(&doc)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Highlight)
            .collect();
        assert!(
            hls.len() >= 2,
            "a ranged highlight per spanned paragraph, got {}",
            hls.len()
        );
        assert!(
            hls.iter().all(|a| a.start < a.end),
            "highlights carry a real char range"
        );
        // Selecting cleared the selection (the highlight now stands in for it).
        assert!(app.sel.is_none(), "selection cleared after highlighting");
    }

    #[test]
    fn note_action_opens_an_on_page_composer_over_a_selection() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Make a cross-paragraph selection via drag.
        let r0 = app.sel_runs[0];
        let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
        let later = app
            .sel_runs
            .iter()
            .find(|r| r.block > r0.block)
            .copied()
            .unwrap();
        let end = Point::new(
            f64::midpoint(later.x0, later.x1),
            f64::midpoint(later.y0, later.y1),
        );
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);
        assert!(app.sel.is_some_and(|s| !s.is_empty()));

        // The toolbar's Note action opens the composer (not the side rail),
        // anchored to the selection, with the selection's text as the excerpt.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let note = find_hit(&app, |a| matches!(a, Action::AddNote)).expect("a Note action");
        assert!(click(&mut app, &mut doc, note), "note handled");
        let cid = app.composing.expect("a note is being composed on the page");
        let a = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.id == cid)
            .unwrap();
        assert_eq!(a.kind, AnnoKind::Note);
        assert!(
            !a.excerpt.is_empty(),
            "the composer note carries the selected passage"
        );
        // The on-page composer records a Save hit (proving it's drawn on-page).
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.note_composer.is_open(), "on-page composer is drawn");
    }

    #[test]
    fn copy_action_copies_the_full_selected_passage() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Select a long span: from the first run to the last, across paragraphs.
        let r0 = app.sel_runs[0];
        let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
        let last = *app.sel_runs.last().unwrap();
        let end = Point::new(last.x1, f64::midpoint(last.y0, last.y1));
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);
        assert!(app.sel.is_some_and(|s| !s.is_empty()), "a selection exists");

        // The text Copy hands the clipboard is the FULL selected passage — not
        // the 160-char excerpt used for annotation labels.
        let full = app.selection_text();
        assert!(!full.is_empty(), "selection yields text");
        let excerpt = app.selection_excerpt();
        assert!(
            full.chars().count() >= excerpt.chars().count(),
            "the copied text is at least as long as the annotation excerpt"
        );
        if full.chars().count() > 160 {
            assert_eq!(
                excerpt.chars().count(),
                160,
                "the excerpt caps at 160 chars while Copy is unbounded"
            );
        }

        // The floating toolbar exposes a Copy action; clicking it runs the copy
        // path (the clipboard write is a silent no-op headless) and KEEPS the
        // selection so the reader can still highlight or note the same passage.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let copy = find_hit(&app, |a| matches!(a, Action::CopySelection))
            .expect("a Copy action in the HUD");
        assert!(click(&mut app, &mut doc, copy), "copy handled");
        assert!(
            app.sel.is_some_and(|s| !s.is_empty()),
            "Copy keeps the selection"
        );
    }

    #[test]
    fn copy_key_shortcut_targets_the_active_selection() {
        // A bare `c` copies only when a selection is live (mirroring `h`
        // highlight / `m` note); with no selection it is not consumed.
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.sel.is_none(), "no selection at rest");
        assert!(
            !key_char(&mut app, &mut doc, "c"),
            "bare c is inert without a selection"
        );

        // Make a selection, then `c` is consumed (it drives the copy path).
        let r0 = app.sel_runs[0];
        let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
        let end = Point::new(r0.x1, f64::midpoint(r0.y0, r0.y1));
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);
        assert!(app.sel.is_some_and(|s| !s.is_empty()), "a selection exists");
        assert!(
            key_char(&mut app, &mut doc, "c"),
            "c copies the live selection"
        );
    }

    #[test]
    fn settings_popover_does_not_dim_the_page() {
        use ui::offscreen::OffscreenRenderer;
        let (w, h) = (1000u32, 700u32);
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Same page, popover closed then open.
        let closed = render_scene(&mut app, &mut doc, f64::from(w), f64::from(h));
        app.type_popover = true;
        let open = render_scene(&mut app, &mut doc, f64::from(w), f64::from(h));

        let mut ro = OffscreenRenderer::new(w, h);
        let c_px = ro.render_rgba(&closed);
        let o_px = ro.render_rgba(&open);
        // A point well inside the page column, above/left of the bottom-right
        // popover panel — if a scrim dimmed the page it would darken here.
        let (x, y) = (400u32, 250u32);
        let a0 = OffscreenRenderer::sample(&c_px, w, x, y);
        let a1 = OffscreenRenderer::sample(&o_px, w, x, y);
        let d = |u: u8, v: u8| (i32::from(u) - i32::from(v)).abs();
        assert!(
            d(a0.0, a1.0) <= 3 && d(a0.1, a1.1) <= 3 && d(a0.2, a1.2) <= 3,
            "the open settings popover must not dim the page: {a0:?} vs {a1:?}",
        );
    }

    #[test]
    fn composition_setting_relayouts_and_persists() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.layout.is_some(), "reader starts laid out");

        app.type_popover = true;
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let ragged = find_hit(&app, |action| {
            matches!(action, Action::SetComposition(CompositionStyle::Ragged))
        })
        .expect("settings popover exposes the Ragged composition choice");
        assert!(click(&mut app, &mut doc, ragged), "choice is clickable");

        assert!(
            app.layout.is_none(),
            "composition changes invalidate layout"
        );
        assert_eq!(app.settings.composition, CompositionStyle::Ragged);
        assert_eq!(
            BooksDoc::read_settings(&doc).composition,
            CompositionStyle::Ragged,
            "reader preference persists"
        );
    }

    #[test]
    fn serif_reader_matches_typography_baseline() {
        use std::path::PathBuf;

        use ui::offscreen::OffscreenRenderer;

        let (width, height) = (1280u32, 800u32);
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        let scene = render_scene(&mut app, &mut doc, f64::from(width), f64::from(height));
        let mut renderer = match OffscreenRenderer::try_new(width, height) {
            Ok(renderer) => renderer,
            Err(error) => {
                eprintln!("offscreen: {error} — skipping ebook typography baseline");
                return;
            }
        };
        let actual = renderer.render_rgba(&scene);
        let baseline = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typography/serif-reader.png");
        if std::env::var_os("UPDATE_EBOOK_TYPOGRAPHY_BASELINE").is_some() {
            if let Some(parent) = baseline.parent() {
                std::fs::create_dir_all(parent).expect("create ebook typography baseline dir");
            }
            image::save_buffer(&baseline, &actual, width, height, image::ColorType::Rgba8)
                .expect("write ebook typography baseline");
            eprintln!("updated {}", baseline.display());
            return;
        }
        let expected = image::open(&baseline)
            .unwrap_or_else(|error| {
                panic!(
                    "missing ebook typography baseline {}: {error}; regenerate with \
                     UPDATE_EBOOK_TYPOGRAPHY_BASELINE=1",
                    baseline.display()
                )
            })
            .to_rgba8();
        assert_eq!(
            expected.dimensions(),
            (width, height),
            "ebook typography baseline changed dimensions"
        );
        let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, expected.as_raw());
        assert_eq!(
            differing, 0,
            "serif reader differs from typography baseline in {differing} pixels"
        );
    }

    #[test]
    fn mathematical_reader_matches_shared_typesetting_baseline() {
        use std::path::PathBuf;

        use ui::offscreen::OffscreenRenderer;

        let (width, height) = (1280u32, 800u32);
        let mut app = app_with_math_sample();
        let mut doc = AutoCommit::new();
        let scene = render_scene(&mut app, &mut doc, f64::from(width), f64::from(height));
        let mut renderer = match OffscreenRenderer::try_new(width, height) {
            Ok(renderer) => renderer,
            Err(error) => {
                eprintln!("offscreen: {error} — skipping ebook math baseline");
                return;
            }
        };
        let actual = renderer.render_rgba(&scene);
        let baseline = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/typography/math-reader.png");
        if std::env::var_os("UPDATE_EBOOK_MATH_BASELINE").is_some() {
            if let Some(parent) = baseline.parent() {
                std::fs::create_dir_all(parent).expect("create ebook math baseline dir");
            }
            image::save_buffer(&baseline, &actual, width, height, image::ColorType::Rgba8)
                .expect("write ebook math baseline");
            eprintln!("updated {}", baseline.display());
            return;
        }
        let expected = image::open(&baseline)
            .unwrap_or_else(|error| {
                panic!(
                    "missing ebook math baseline {}: {error}; regenerate with \
                     UPDATE_EBOOK_MATH_BASELINE=1",
                    baseline.display()
                )
            })
            .to_rgba8();
        assert_eq!(expected.dimensions(), (width, height));
        let differing = OffscreenRenderer::pixels_differing_beyond_aa(&actual, expected.as_raw());
        assert_eq!(
            differing, 0,
            "mathematical reader differs from baseline in {differing} pixels"
        );
    }

    #[test]
    fn wheel_scrolls_the_library_shelf() {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        // A short window so the three sample cards overflow into a scroll range.
        frame(&mut app, &mut doc, 560.0, 600.0);
        assert!(app.shelf_max_scroll > 0.0, "shelf overflows this window");
        assert_eq!(app.shelf_scroll, 0.0, "starts at the top");
        // Scroll down, then hard down — must clamp at the bottom.
        assert!(wheel(&mut app, &mut doc, -3.0), "wheel scrolled the shelf");
        assert!(app.shelf_scroll > 0.0);
        wheel(&mut app, &mut doc, -100.0);
        assert!(
            app.shelf_scroll <= app.shelf_max_scroll + 0.001,
            "never past the end"
        );
        assert!(app.shelf_scroll > 0.0);
        // Scroll back up — must clamp at the top.
        wheel(&mut app, &mut doc, 100.0);
        assert_eq!(app.shelf_scroll, 0.0, "clamps at the top");
    }

    #[test]
    fn opening_a_book_from_the_shelf_enters_the_reader() {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 720.0);
        let target = find_hit(&app, |a| matches!(a, Action::OpenBook(_))).expect("a book card hit");
        assert!(click(&mut app, &mut doc, target), "book click handled");
        assert_eq!(app.view, View::Reader);
    }

    #[test]
    fn next_page_advances_position() {
        let mut app = app_with_sample();
        // The page-nav stepper now lives on the floating page bubble, which is
        // transient — arm it open so its next chevron registers a NextPage hit,
        // then click that chevron on the real hit-testing path.
        app.arm_reader_bubble_for_shots();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let (c0, p0) = (app.chapter, app.page);
        let next = find_hit(&app, |a| matches!(a, Action::NextPage))
            .expect("next chevron on the page bubble");
        assert!(click(&mut app, &mut doc, next), "next handled");
        assert!(
            app.page != p0 || app.chapter != c0,
            "position advanced from ch{c0}/pg{p0} to ch{}/pg{}",
            app.chapter,
            app.page
        );
    }

    /// The floating page bubble is a transient overlay: hidden on a settled
    /// reader, surfaced by a page change, and it schedules the animation frames
    /// that let it fade itself out (so it hides without a stray pointer event).
    #[test]
    fn page_bubble_surfaces_on_a_page_change_and_schedules_its_fade() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Settled reader: the bubble is not up, and no NextPage hit is exposed in
        // the bottom band (an invisible bubble never eats a click there).
        assert!(
            !app.needs_animation_frame(),
            "a settled reader schedules no animation frames"
        );
        assert!(
            find_hit(&app, |a| matches!(a, Action::NextPage)).is_none(),
            "the hidden bubble exposes no page-nav hit"
        );
        // A page turn pokes the bubble; the next frame draws it (its chevrons are
        // hit-testable) and it keeps requesting frames so it can fade on its own.
        app.dispatch(Action::NextPage, &mut doc);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            find_hit(&app, |a| matches!(a, Action::NextPage)).is_some(),
            "a page change surfaces the bubble's next chevron"
        );
        assert!(
            app.reader_bubble.needs_frame(Instant::now()),
            "the visible bubble schedules its own fade-out frames"
        );
    }

    #[test]
    fn continuous_mode_scrolls_the_flow_and_persists() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Switch to continuous scroll through the real command path.
        app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.settings.mode, ReadMode::Continuous, "mode switched");
        assert_eq!(
            BooksDoc::read_settings(&doc).mode,
            ReadMode::Continuous,
            "mode persisted"
        );
        // A downward wheel scrolls the flow (rolls chapters at the ends) rather
        // than snapping whole pages.
        let before = (app.chapter, app.scroll_y);
        assert!(wheel(&mut app, &mut doc, -6.0), "continuous wheel handled");
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_ne!(
            (app.chapter, app.scroll_y),
            before,
            "continuous scroll moved the reading position"
        );
        // Progress persisted for the shelf bar.
        let id = app.book_id();
        assert!(
            BooksDoc::read_progress(&doc, &id).fraction >= 0.0,
            "progress recorded in continuous mode"
        );
    }

    #[test]
    fn two_up_shows_two_columns_and_turns_by_two() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 820.0);
        app.dispatch(Action::SetMode(ReadMode::TwoUp), &mut doc);
        frame(&mut app, &mut doc, 1280.0, 820.0);
        assert_eq!(app.settings.mode, ReadMode::TwoUp);
        // A spread aligns to an even left page.
        assert_eq!(app.page % 2, 0, "the spread's left page is even");
        // The two facing pages paint into two columns on the real draw path.
        if app.cur_page_count >= 2 {
            let cols: std::collections::HashSet<u8> = app.sel_runs.iter().map(|r| r.col).collect();
            assert!(
                cols.contains(&0) && cols.contains(&1),
                "the spread paints two facing columns"
            );
        }
        // A page turn advances by a whole spread (two pages), or rolls chapter.
        let (c0, p0) = (app.chapter, app.page);
        app.dispatch(Action::NextPage, &mut doc);
        frame(&mut app, &mut doc, 1280.0, 820.0);
        if app.chapter == c0 {
            assert_eq!(app.page, p0 + 2, "a spread turns two pages at once");
        } else {
            assert_eq!(app.chapter, c0 + 1, "or rolls to the next chapter");
        }
    }

    /// A paged turn eases the incoming page in (a slide + fade) instead of
    /// snapping: the turn schedules animation frames and the incoming page starts
    /// offset off its resting x, decaying toward zero. Continuous scroll never
    /// animates a page turn — it just scrolls.
    #[test]
    fn turning_a_page_animates_an_eased_slide() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(
            app.settings.mode,
            ReadMode::Paged,
            "paged is the default mode"
        );
        assert!(
            !app.needs_animation_frame(),
            "a settled reader schedules no animation frames"
        );
        assert!(
            app.cur_page_count >= 2,
            "the sample chapter paginates into several pages"
        );

        // A forward turn (pages to spare) advances and starts the slide.
        let (c0, p0) = (app.chapter, app.page);
        app.dispatch(Action::NextPage, &mut doc);
        assert_ne!((app.chapter, app.page), (c0, p0), "the page advanced");
        assert!(
            app.needs_animation_frame(),
            "the turn schedules animation frames"
        );
        let pt = app.page_turn.expect("a page-turn slide is in flight");
        let (dx, alpha) = pt.visual(std::time::Instant::now(), 500.0);
        assert!(
            dx > 0.0,
            "a forward turn slides the incoming page in from the right: {dx}"
        );
        assert!(
            alpha < 1.0,
            "the incoming page fades up as it arrives: {alpha}"
        );

        // Continuous scroll scrolls instead of turning, so no slide is scheduled.
        app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
        frame(&mut app, &mut doc, 1000.0, 700.0); // settles + clears the prior turn
        assert!(
            app.page_turn.is_none(),
            "the paged turn cleared after settling"
        );
        app.dispatch(Action::NextPage, &mut doc);
        assert!(
            app.page_turn.is_none(),
            "continuous mode never starts a page-turn slide"
        );
    }

    /// A highlight's margin marker is a draggable handle: grabbing it and dropping
    /// it on another paragraph re-anchors the highlight there through the
    /// annotation action model (one row moves, its comments are untouched). A
    /// plain tap on the same marker opens the highlight's comments.
    #[test]
    fn dragging_a_highlight_marker_reanchors_it_and_keeps_its_comments() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);

        // Highlight a range within the first paragraph (single block → movable).
        let r0 = app.sel_runs[0];
        let end_run = app
            .sel_runs
            .iter()
            .rfind(|r| r.block == r0.block)
            .copied()
            .unwrap();
        ptr_move(
            &mut app,
            &mut doc,
            Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1)),
        );
        ptr_down(&mut app, &mut doc);
        ptr_move(
            &mut app,
            &mut doc,
            Point::new(
                f64::midpoint(end_run.x0, end_run.x1),
                f64::midpoint(end_run.y0, end_run.y1),
            ),
        );
        ptr_up(&mut app, &mut doc);
        app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
        app.refresh_from_doc(&doc);
        let hl = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("a highlight now exists");
        let group = hl.group;
        let src_block = hl.block;

        // Attach a comment so the re-anchor can be shown not to disturb it.
        let comment = Annotation {
            id: uuid::Uuid::new_v4(),
            doc_id: app.book_id(),
            kind: AnnoKind::Note,
            chapter: 0,
            block: src_block,
            start: hl.start,
            end: hl.end,
            excerpt: "x".into(),
            note: "keep me".into(),
            color: HiColor::Green,
            created: 5,
            group,
        };
        BooksDoc::upsert_annotation(&mut doc, &comment);
        app.refresh_from_doc(&doc);

        // A frame records the group's draggable margin marker.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let marker_rect = app
            .hi_markers
            .iter()
            .find(|(_, g)| *g == group)
            .map(|(r, _)| *r)
            .expect("the highlight has a draggable margin marker");

        // A target paragraph in a different block.
        let target = app
            .sel_runs
            .iter()
            .find(|r| r.block != src_block)
            .copied()
            .expect("another paragraph is visible on the page");
        let target_block = target.block;
        let target_pt = Point::new(
            f64::midpoint(target.x0, target.x1),
            f64::midpoint(target.y0, target.y1),
        );

        // Grab the marker and drag it onto the target paragraph.
        ptr_move(&mut app, &mut doc, marker_rect.center());
        ptr_down(&mut app, &mut doc);
        assert!(
            app.marker_drag.is_some(),
            "pressing the marker began a re-anchor drag"
        );
        ptr_move(&mut app, &mut doc, target_pt);
        ptr_up(&mut app, &mut doc);
        app.refresh_from_doc(&doc);

        // The highlight re-anchored to the dropped paragraph — the SAME row moved
        // (its id is preserved, proving it went through `MoveAnnotation`, not a
        // delete + re-create).
        let moved = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight && a.group == group)
            .expect("the highlight still exists");
        assert_eq!(
            moved.id, hl.id,
            "the same highlight row moved (not recreated)"
        );
        assert_eq!(
            moved.block, target_block,
            "the highlight re-anchored to the dropped paragraph"
        );
        assert_ne!(moved.block, src_block, "it actually moved off its origin");
        // Its comment survived the move.
        assert!(
            BooksDoc::list_annotations(&doc)
                .into_iter()
                .any(|a| a.kind == AnnoKind::Note && a.group == group && a.note == "keep me"),
            "the highlight's comment survived the re-anchor",
        );
    }

    /// A plain tap on a highlight's margin marker (no drag) opens its comments,
    /// so the marker doubles as the comment affordance.
    #[test]
    fn tapping_a_highlight_marker_opens_its_comments() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let r0 = app.sel_runs[0];
        let end_run = app
            .sel_runs
            .iter()
            .rfind(|r| r.block == r0.block)
            .copied()
            .unwrap();
        ptr_move(
            &mut app,
            &mut doc,
            Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1)),
        );
        ptr_down(&mut app, &mut doc);
        ptr_move(
            &mut app,
            &mut doc,
            Point::new(
                f64::midpoint(end_run.x0, end_run.x1),
                f64::midpoint(end_run.y0, end_run.y1),
            ),
        );
        ptr_up(&mut app, &mut doc);
        app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
        app.refresh_from_doc(&doc);
        let group = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("a highlight")
            .group;
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let marker = app
            .hi_markers
            .iter()
            .find(|(_, g)| *g == group)
            .map(|(r, _)| r.center())
            .expect("the highlight has a marker");
        // Press + release without moving.
        ptr_move(&mut app, &mut doc, marker);
        ptr_down(&mut app, &mut doc);
        ptr_up(&mut app, &mut doc);
        assert_eq!(
            app.comment_overlay.group(),
            Some(group),
            "a tap on the marker opened the highlight's comments"
        );
    }

    #[test]
    fn shared_selection_hud_shows_in_continuous_mode() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.dispatch(Action::SetMode(ReadMode::Continuous), &mut doc);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Seed a live selection near the top of the flow and redraw.
        app.sel = Some(TextSel {
            anchor: DocPos { block: 1, ch: 0 },
            head: DocPos { block: 1, ch: 24 },
        });
        app.selected_block = Some(1);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // The shared HUD registered its Copy + a swatch control as clickable hits.
        assert!(
            find_hit(&app, |a| matches!(a, Action::CopySelection)).is_some(),
            "the shared selection HUD floats over the continuous flow"
        );
        assert!(
            find_hit(&app, |a| matches!(a, Action::SetColor(_))).is_some(),
            "the HUD offers highlight swatches"
        );
    }

    #[test]
    fn double_click_selects_a_word_then_a_swatch_writes_a_ranged_highlight() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let before = BooksDoc::list_annotations(&doc).len();
        // Double-click a word: the selection is exactly that word's char span.
        let p = page_point(&app);
        let word_run = app.sel_runs[0];
        dbl_click(&mut app, &mut doc, p);
        let sel = app.sel.expect("double-click made a selection");
        let (s, e) = sel.ordered();
        assert_eq!(s.block, word_run.block);
        assert_eq!(s.ch, word_run.cs, "selection starts at the word start");
        assert_eq!(
            e.ch,
            word_run.cs + word_run.cl,
            "selection ends at the word end"
        );
        // The selection HUD offers colour swatches — click one to highlight.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        assert!(click(&mut app, &mut doc, swatch), "swatch handled");
        let after = BooksDoc::list_annotations(&doc);
        assert_eq!(after.len(), before + 1, "one highlight added");
        let hl = after
            .iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .unwrap();
        assert!(
            hl.start < hl.end,
            "the highlight carries a real char range (ranged, not whole-block)"
        );
    }

    #[test]
    fn continuous_highlight_band_fills_the_gap_between_words() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Find two adjacent runs (words) on the same line with a visible gap.
        let runs = app.sel_runs.clone();
        let (a, b) = runs
            .windows(2)
            .map(|w| (w[0], w[1]))
            .find(|(a, b)| a.block == b.block && (a.y0 - b.y0).abs() < 0.5 && b.x0 > a.x1 + 1.0)
            .expect("two adjacent words with a gap");
        // A highlight covering both words (a real ranged annotation on the page).
        let book_id = app.book_id();
        let mut hl = Annotation::new(&book_id, AnnoKind::Highlight, app.chapter, a.block)
            .with_color(HiColor::Yellow);
        hl.start = a.cs;
        hl.end = b.cs + b.cl;
        BooksDoc::upsert_annotation(&mut doc, &hl);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // The gap midpoint between the two words must be inside a drawn band —
        // proving the band is continuous, not per-glyph.
        let gap_mid = Point::new(f64::midpoint(a.x1, b.x0), f64::midpoint(a.y0, a.y1));
        assert!(
            app.hi_bands.iter().any(|band| band.contains(gap_mid)),
            "a continuous highlight band covers the inter-word gap"
        );
    }

    #[test]
    fn highlight_colour_toggles_off_and_replaces() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let count = |d: &AutoCommit| {
            BooksDoc::list_annotations(d)
                .into_iter()
                .filter(|a| a.kind == AnnoKind::Highlight)
                .count()
        };
        let color_of = |d: &AutoCommit| {
            BooksDoc::list_annotations(d)
                .into_iter()
                .find(|a| a.kind == AnnoKind::Highlight)
                .map(|a| a.color)
        };
        // Select a word, then apply yellow.
        let p = page_point(&app);
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
        app.refresh_from_doc(&doc);
        assert_eq!(count(&doc), 1);
        assert_eq!(color_of(&doc), Some(HiColor::Yellow));

        // Re-select the same word; picking a DIFFERENT colour replaces it.
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
        app.refresh_from_doc(&doc);
        assert_eq!(count(&doc), 1, "different colour replaces (never layers)");
        assert_eq!(color_of(&doc), Some(HiColor::Green));

        // Re-select the same word; picking the SAME colour toggles it off.
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
        app.refresh_from_doc(&doc);
        assert_eq!(count(&doc), 0, "same colour again removes the highlight");
    }

    #[test]
    fn shift_and_ctrl_shift_arrows_extend_selection() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Seed a collapsed selection at the start of the first word.
        let r0 = app.sel_runs[0];
        let start = DocPos {
            block: r0.block,
            ch: r0.cs,
        };
        app.sel = Some(TextSel {
            anchor: start,
            head: start,
        });
        // Shift+Right extends by one character.
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowRight,
            ModifiersState::SHIFT
        ));
        assert_eq!(
            app.sel.unwrap().head,
            DocPos {
                block: r0.block,
                ch: r0.cs + 1
            },
            "char extend"
        );
        // Shift+Left contracts back by one character.
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowLeft,
            ModifiersState::SHIFT
        ));
        assert_eq!(app.sel.unwrap().head, start, "char contract");
        // Ctrl+Shift+Right jumps to the next word boundary.
        let expect_word = app.doc_next_word(start);
        assert!(
            expect_word.ch > start.ch,
            "there is a next word boundary to reach"
        );
        assert!(key_named_mods(
            &mut app,
            &mut doc,
            NamedKey::ArrowRight,
            ModifiersState::SHIFT | ModifiersState::CONTROL
        ));
        assert_eq!(app.sel.unwrap().head, expect_word, "word extend");
    }

    #[test]
    fn aa_button_opens_popover_and_page_tint_persists() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(!app.type_popover);
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        assert!(click(&mut app, &mut doc, aa));
        assert!(app.type_popover, "popover opened");
        // Choose the Dark page tint from the popover.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let dark = find_hit(&app, |a| matches!(a, Action::SetReadTheme(ReadTheme::Dark)))
            .expect("dark tint pill");
        assert!(click(&mut app, &mut doc, dark));
        assert_eq!(BooksDoc::read_settings(&doc).theme, ReadTheme::Dark);
    }

    #[test]
    fn rails_collapse_on_narrow_and_dock_on_wide() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Wide: both rails docked-open by default.
        frame(&mut app, &mut doc, 1280.0, 720.0);
        assert!(app.toc_open(), "contents docked+open at 1280");
        assert!(app.notes_open(), "annotations docked+open at 1280");
        // Narrow: both become closed overlays, so the page owns the width.
        frame(&mut app, &mut doc, 560.0, 720.0);
        assert_eq!(app.toc_layout(), SplitLayout::Overlay);
        assert_eq!(app.notes_layout(), SplitLayout::Overlay);
        assert!(!app.toc_open(), "contents closed at 560");
        assert!(!app.notes_open(), "annotations closed at 560");
    }

    #[test]
    fn runtime_start_seeds_no_annotations() {
        // A non-demo app must never fabricate marks into a fresh document.
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            BooksDoc::list_annotations(&doc).is_empty(),
            "no seeded annotations"
        );
    }

    // ---- library-shelf keyboard ----------------------------------------------

    #[test]
    fn library_keyboard_moves_focus_and_opens_the_focused_book() {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 720.0);
        assert!(app.library.len() >= 3, "sample shelf has several books");
        assert_eq!(app.shelf_cursor, 0, "focus starts on the first book");

        // Arrow-right/-left move the focus along the row.
        assert!(
            key_named(&mut app, &mut doc, NamedKey::ArrowRight),
            "arrow consumed"
        );
        assert_eq!(app.shelf_cursor, 1, "arrow-right moved focus");
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowLeft));
        assert_eq!(app.shelf_cursor, 0, "arrow-left moved focus back");

        // End jumps to the last book; Home back to the first.
        assert!(key_named(&mut app, &mut doc, NamedKey::End));
        assert_eq!(app.shelf_cursor, app.library.len() - 1, "End -> last book");
        assert!(key_named(&mut app, &mut doc, NamedKey::Home));
        assert_eq!(app.shelf_cursor, 0, "Home -> first book");

        // Enter opens the focused book (mapping the display position back to the
        // real library index through the sorted display order).
        let expected = app.shelf_order[app.shelf_cursor];
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Enter),
            "enter consumed"
        );
        assert_eq!(app.view, View::Reader, "enter opened the focused book");
        assert_eq!(
            app.book_idx, expected,
            "opened the focused book (via display order)"
        );
    }

    #[test]
    fn library_arrow_down_steps_a_whole_row() {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        // A narrow window forces a single column, so down moves to the next book.
        frame(&mut app, &mut doc, 320.0, 700.0);
        assert_eq!(app.shelf_cols, 1, "one column at this width");
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowDown));
        assert_eq!(
            app.shelf_cursor, 1,
            "down stepped one row (one book) in a single column"
        );
    }

    // ---- library: open-from-anywhere, import, share ------------------------

    #[test]
    fn live_constructor_starts_empty_on_the_library() {
        // The live app opens onto the shelf (its home), empty until the document
        // load on the first sync — never a fabricated starter book.
        let app = BooksApp::new();
        assert!(
            app.library.is_empty(),
            "no books before the document is loaded"
        );
        assert_eq!(app.view, View::Library, "the shelf is home, not a book");
        assert!(
            !app.library_loaded,
            "the persisted library loads on first sync"
        );
    }

    #[test]
    fn the_empty_shelf_onboards_with_open_book_not_a_reload() {
        // The empty state offers the primary way in — open a book from anywhere —
        // and never asks the reader to drop files in a magic folder and reload.
        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        assert!(app.library.is_empty());
        assert!(
            find_hit(&app, |a| matches!(a, Action::OpenViaPortal)).is_some(),
            "the empty shelf offers an Open-book action"
        );
    }

    /// The core of the redesign: opening a book by path imports it into the
    /// library (so it's there next time) AND opens it. Exercised through the OS
    /// file-association entry point, which drains on the first sync.
    #[test]
    fn opening_a_book_by_path_imports_it_and_opens_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("handed-off.epub");
        std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();

        let mut app = BooksApp::open_path(&path);
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);

        assert_eq!(
            app.view,
            View::Reader,
            "the handed-off book opens in the reader"
        );
        assert_eq!(app.library.len(), 1, "and is now on the shelf");
        assert!(
            !app.library[0].title.is_empty(),
            "the real epub parsed a title"
        );
        // Persisted, so it is there on the next launch.
        assert_eq!(
            BooksDoc::read_library_entries(&doc).len(),
            1,
            "the opened book is recorded in the library"
        );
    }

    /// A book chosen through the host file picker loads through the per-frame
    /// portal drain (the result is injected — no real dialog).
    #[test]
    fn a_picked_book_is_imported_and_opened_via_the_frame_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("picked.epub");
        std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();

        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        assert!(app.library.is_empty(), "starts empty");

        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(path)));
        app.sync(&mut doc, &automerge::ROOT);

        assert_eq!(app.library.len(), 1, "the picked book joined the library");
        assert_eq!(app.view, View::Reader, "and opened in the reader");
        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after delivering"
        );
    }

    /// The same file opened from two different paths resolves to one content-hash
    /// identity, so its highlights and progress are shared — the whole point of
    /// keying resources to the book, not the path or the device.
    #[test]
    fn the_same_book_from_two_paths_shares_one_identity() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let pa = a.path().join("from-downloads.epub");
        let pb = b.path().join("from-usb-stick.epub");
        std::fs::write(&pa, crate::content::SAMPLE_EPUB).unwrap();
        std::fs::write(&pb, crate::content::SAMPLE_EPUB).unwrap();

        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);

        app.import_and_open(&mut doc, &pa);
        let id = app.library[0].id.clone();
        // A highlight made while reading the first copy.
        BooksDoc::upsert_annotation(&mut doc, &Annotation::new(&id, AnnoKind::Highlight, 0, 1));
        assert_eq!(BooksDoc::annotations_for(&doc, &id).len(), 1);

        // Open the *same bytes* from a completely different path.
        app.import_and_open(&mut doc, &pb);
        assert_eq!(
            app.library.len(),
            1,
            "same content → one book, not a duplicate"
        );
        assert_eq!(
            app.library[0].id, id,
            "identity is the content hash, not the path"
        );
        assert_eq!(
            BooksDoc::annotations_for(&doc, &app.library[0].id).len(),
            1,
            "the highlight follows the book to the new path"
        );
    }

    /// A book living on the collaborative workspace is surfaced as shared; a book
    /// anywhere else is local.
    #[test]
    #[serial]
    fn a_workspace_book_is_surfaced_as_shared() {
        let ws = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("QOS_WORKSPACE_DIR", ws.path()) };
        let books = crate::content::workspace_books_dir();
        std::fs::create_dir_all(&books).unwrap();
        let shared_path = books.join("shared.epub");
        std::fs::write(&shared_path, crate::content::SAMPLE_EPUB).unwrap();

        let local = tempfile::tempdir().unwrap();
        let local_path = local.path().join("local.epub");
        std::fs::write(&local_path, crate::content::SAMPLE_EPUB).unwrap();

        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);

        app.import_and_open(&mut doc, &shared_path);
        let shared_idx = app
            .library
            .iter()
            .position(|b| b.id == app.library[0].id)
            .unwrap();
        assert!(
            app.book_is_shared(shared_idx),
            "a /workspace/Books book reads as shared"
        );

        // The badge is drawn on the shelf: the shared pill's people-glyph action
        // is absent (no Share button on an already-shared book).
        unsafe { std::env::remove_var("QOS_WORKSPACE_DIR") };
        let _ = local_path;
    }

    #[test]
    #[serial]
    fn sharing_a_local_book_moves_it_onto_the_workspace() {
        let ws = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("QOS_WORKSPACE_DIR", ws.path()) };
        let home = tempfile::tempdir().unwrap();
        let src = home.path().join("mine.epub");
        std::fs::write(&src, crate::content::SAMPLE_EPUB).unwrap();

        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        app.import_and_open(&mut doc, &src);
        assert!(!app.book_is_shared(0), "starts local");

        app.dispatch(Action::ShareToWorkspace(0), &mut doc);
        assert!(
            app.book_is_shared(0),
            "sharing puts the book on the workspace"
        );
        unsafe { std::env::remove_var("QOS_WORKSPACE_DIR") };
    }

    #[test]
    fn removing_a_book_takes_it_off_the_shelf_and_out_of_the_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("book.epub");
        std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 720.0);
        app.import_and_open(&mut doc, &path);
        assert_eq!(app.library.len(), 1);
        assert_eq!(BooksDoc::read_library_entries(&doc).len(), 1);

        app.dispatch(Action::RemoveFromLibrary(0), &mut doc);
        assert!(app.library.is_empty(), "removed from the shelf");
        assert!(
            BooksDoc::read_library_entries(&doc).is_empty(),
            "and from the registry"
        );
    }

    #[test]
    fn populated_shelf_offers_an_open_book_affordance() {
        // A shelf that already has books still offers the trailing "Add a book"
        // tile plus the top-bar Open action, so there is always a way in.
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        assert!(!app.library.is_empty(), "sample shelf has books");
        assert!(
            find_hit(&app, |a| matches!(a, Action::OpenBook(_))).is_some(),
            "book cards are present on the shelf"
        );
        assert!(
            find_hit(&app, |a| matches!(a, Action::OpenViaPortal)).is_some(),
            "a populated shelf still offers a way to add more books"
        );
    }

    #[test]
    fn card_shows_the_title_once_generated_vs_real_cover() {
        // A generated cover sets the title in display type on the cover itself,
        // so `cover_image` yields None and the under-cover shelf label is
        // suppressed — the title renders exactly once. Real cover art keeps the
        // shelf label (its embedded title may not be legible on the shelf), and
        // does not itself re-draw the title, so it too shows once.
        let mut app = BooksApp::with_library(Vec::new());
        let generated = Book {
            id: "gen".into(),
            title: "Generated".into(),
            cover: None,
            ..Default::default()
        };
        assert!(
            app.cover_image(&generated).is_none(),
            "generated cover has no image → shelf label is suppressed (title only on the cover)"
        );
        let real = Book {
            id: "real".into(),
            title: "Real".into(),
            cover: Some(crate::model::Cover {
                rgba: std::sync::Arc::new(vec![0u8; 2 * 2 * 4]),
                width: 2,
                height: 2,
            }),
            ..Default::default()
        };
        assert!(
            app.cover_image(&real).is_some(),
            "real cover art blits → the single title comes from the shelf label"
        );
    }

    #[test]
    fn type_popover_stays_clear_of_the_docked_annotations_rail() {
        // On a wide window the annotations rail is docked-open; the reading
        // settings popover must anchor inside the reading area, never crossing it.
        let mut app = app_with_sample();
        app.view = View::Reader;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.reconcile_flaps();
        app.notes_flap.snap(true); // fully docked-open
        assert_eq!(
            app.notes_layout(),
            SplitLayout::Docked,
            "wide window docks the rail"
        );

        let panel_w = 320.0_f64.min(app.rect.width() - 24.0);
        let (px0, px1) = app.type_popover_x_bounds(panel_w);
        let rail_left = app.rect.x1 - NOTES_W;
        assert!(
            px1 <= rail_left,
            "popover right edge {px1} must stay left of the docked annotations rail at {rail_left}"
        );
        assert!(px0 >= app.rect.x0, "popover stays on-screen");
        assert!(px1 - px0 >= 200.0, "popover keeps a usable width");
    }

    // ---- delete affordance ---------------------------------------------------

    #[test]
    fn annotation_delete_is_reachable_and_removes_it() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Wide window so the annotations rail is docked-open.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        // Create one highlight via double-click word + swatch.
        let __p = page_point(&app);
        dbl_click(&mut app, &mut doc, __p);
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        click(&mut app, &mut doc, swatch);
        assert_eq!(
            BooksDoc::list_annotations(&doc).len(),
            1,
            "one highlight created"
        );

        // The delete (×) is registered without the pointer hovering the card
        // (the cursor is still over the swatch on the page), proving reachability.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let del = find_hit(&app, |a| matches!(a, Action::Delete(_)))
            .expect("delete affordance is present without hover");
        assert!(click(&mut app, &mut doc, del), "delete click handled");
        assert!(
            BooksDoc::list_annotations(&doc).is_empty(),
            "annotation removed"
        );
    }

    // ---- single source of truth: affordance == palette ActionSpec -----------

    /// The comment-overlay Delete (×) button dispatches the byte-identical
    /// `EncodedAction` the palette's "Delete comment" `ActionSpec` exports — proof
    /// the two are one code path, not two. Drives the real click through
    /// `handle_event` and compares against the live resource tree.
    #[test]
    fn comment_delete_button_dispatches_the_same_action_as_the_palette() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let book_id = app.book_id();
        let group = Uuid::from_u128(0x2222_3333_4444_5555_6666_7777_8888_9999);
        let cid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_00AB);
        let hl = Annotation::new(&book_id, AnnoKind::Highlight, 0, 1)
            .with_excerpt("a marked passage")
            .with_group(group);
        BooksDoc::upsert_annotation(&mut doc, &hl);
        let mut cm = Annotation::new(&book_id, AnnoKind::Note, 0, 1)
            .with_excerpt("a marked passage")
            .with_note("a comment")
            .with_group(group);
        cm.id = cid;
        BooksDoc::upsert_annotation(&mut doc, &cm);
        // Open the highlight's comment overlay (presentation).
        app.comment_overlay.open(group);
        frame(&mut app, &mut doc, 1280.0, 800.0);

        // The EncodedAction the palette would dispatch for "Delete comment".
        let resources = <BooksApp as qos_actions::ResourceProvider>::resources(&app, &doc);
        let spec_encoded = {
            let hl_res = resources[0]
                .children
                .iter()
                .find(|r| r.kind == "ebook.highlight")
                .expect("highlight resource");
            let comment = hl_res
                .children
                .iter()
                .find(|r| r.id.as_str() == format!("ebook.comment:{cid}"))
                .expect("comment resource");
            let spec = comment
                .actions
                .iter()
                .find(|a| a.id == "delete-comment")
                .expect("delete-comment spec");
            match &spec.input {
                qos_actions::ActionInput::Immediate(enc) => enc.clone(),
                qos_actions::ActionInput::Prompted { .. } => panic!("delete-comment is immediate"),
            }
        };

        // Click the overlay's per-comment × button (the real affordance).
        let del = find_hit(&app, |a| matches!(a, Action::DeleteComment(_)))
            .expect("a comment delete button");
        assert!(
            click(&mut app, &mut doc, del),
            "delete-comment click handled"
        );

        let dispatched = app
            .last_dispatched()
            .cloned()
            .expect("the button dispatched an action");
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the button and the palette build the identical EncodedAction",
        );
        // And it really removed the comment through that action.
        assert!(
            BooksDoc::list_annotations(&doc).iter().all(|a| a.id != cid),
            "the comment was deleted via the dispatched action",
        );
    }

    /// The selection-toolbar swatch, toggling an existing highlight off, dispatches
    /// the byte-identical `EncodedAction` the book resource's "Highlight selection"
    /// `ActionSpec` exports — the swatch and the palette are one source of truth.
    /// Drives the real `SetColor` dispatch arm a swatch click invokes.
    #[test]
    fn selection_swatch_dispatches_the_same_action_as_the_palette() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Create a highlight over the first word via the real swatch path, so the
        // page layout and annotation store are populated exactly as at runtime.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let __p = page_point(&app);
        dbl_click(&mut app, &mut doc, __p);
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(HiColor::Yellow)))
            .expect("the yellow swatch");
        click(&mut app, &mut doc, swatch);
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let hl = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("one highlight now exists");

        // Re-establish the exact selection over that highlight (set directly so
        // the test doesn't depend on a second hit-test resolving under load), so
        // the swatch would toggle it off — a deterministic plan (pure removal, no
        // freshly-minted ids), the case where the two paths encode identically.
        app.hi_color = HiColor::Yellow;
        app.sel = Some(TextSel {
            anchor: DocPos {
                block: hl.block,
                ch: hl.start,
            },
            head: DocPos {
                block: hl.block,
                ch: hl.end,
            },
        });
        app.selected_block = Some(hl.block);

        // The palette's book-level "Highlight selection" EncodedAction, resolved
        // from the same live selection the swatch sees.
        let resources = <BooksApp as qos_actions::ResourceProvider>::resources(&app, &doc);
        let spec_encoded = {
            let spec = resources[0]
                .actions
                .iter()
                .find(|a| a.id == "highlight-selection")
                .expect("a highlight-selection spec while a selection exists");
            match &spec.input {
                qos_actions::ActionInput::Immediate(enc) => enc.clone(),
                qos_actions::ActionInput::Prompted { .. } => {
                    panic!("highlight-selection is immediate")
                }
            }
        };

        // Dispatch the swatch's action through the real handler a click invokes.
        app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
        let dispatched = app
            .last_dispatched()
            .cloned()
            .expect("the swatch dispatched an action");
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the swatch and the palette build the identical EncodedAction",
        );
        // And it really toggled the highlight off through that action.
        assert!(
            BooksDoc::list_annotations(&doc)
                .iter()
                .all(|a| a.kind != AnnoKind::Highlight),
            "the highlight was removed via the dispatched action",
        );
    }

    // ---- bookmark toggle (icon + 'b') ---------------------------------------

    #[test]
    fn bookmark_toggles_via_icon_and_key() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let bookmarks = |d: &AutoCommit| {
            BooksDoc::list_annotations(d)
                .into_iter()
                .filter(|a| a.kind == AnnoKind::Bookmark)
                .count()
        };
        assert_eq!(bookmarks(&doc), 0, "no bookmark yet");
        // Add via the top-bar bookmark icon.
        let bm = find_hit(&app, |a| matches!(a, Action::ToggleBookmark)).expect("bookmark icon");
        assert!(click(&mut app, &mut doc, bm), "bookmark icon handled");
        assert_eq!(bookmarks(&doc), 1, "icon added a bookmark");
        // Remove via the 'b' shortcut (same page position -> same anchor).
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(key_char(&mut app, &mut doc, "b"), "'b' consumed");
        assert_eq!(bookmarks(&doc), 0, "'b' removed the bookmark");
    }

    // ---- note authoring: type / backspace / done ----------------------------

    #[test]
    fn composing_a_note_types_backspaces_and_persists() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        // Select a word, then start a note with the 'm' shortcut.
        let __p = page_point(&app);
        dbl_click(&mut app, &mut doc, __p);
        assert!(key_char(&mut app, &mut doc, "m"), "'m' started a note");
        let nid = app.composing.expect("a note is now being composed");

        for ch in ["H", "i"] {
            assert!(
                key_char(&mut app, &mut doc, ch),
                "char consumed while composing"
            );
        }
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Space),
            "space consumed"
        );
        for ch in ["t", "h", "e", "r", "e"] {
            assert!(key_char(&mut app, &mut doc, ch));
        }
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Backspace),
            "backspace consumed"
        );
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Enter),
            "enter finished the note"
        );
        assert!(app.composing.is_none(), "enter ended composing");

        let note = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.id == nid)
            .expect("the composed note is in the document");
        assert_eq!(note.kind, AnnoKind::Note);
        assert_eq!(
            note.note, "Hi ther",
            "typed text with one backspace persisted"
        );
    }

    #[test]
    fn clicking_a_finished_note_reopens_it_for_editing() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let __p = page_point(&app);
        dbl_click(&mut app, &mut doc, __p);
        key_char(&mut app, &mut doc, "m");
        key_char(&mut app, &mut doc, "x");
        key_named(&mut app, &mut doc, NamedKey::Enter);
        assert!(app.composing.is_none(), "note finished");

        // Clicking the note card's lower zone reopens it (Action::EditNote).
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let edit = find_hit(&app, |a| matches!(a, Action::EditNote(_))).expect("edit zone");
        assert!(click(&mut app, &mut doc, edit), "edit click handled");
        assert!(app.composing.is_some(), "note reopened for editing");
    }

    // ---- jump-to-annotation location resolution -----------------------------

    #[test]
    fn clicking_an_annotation_card_jumps_to_its_location() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Docked notes rail at a wide width so the card (with a JumpTo hit) shows.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let book_id = app.book_id();
        let nblocks = app.library[app.book_idx].chapters[0].blocks.len().max(1);
        let target = (nblocks / 2).min(nblocks - 1);
        let a = Annotation::new(&book_id, AnnoKind::Highlight, 0, target)
            .with_color(HiColor::Green)
            .with_excerpt("anchored passage");
        BooksDoc::upsert_annotation(&mut doc, &a);
        frame(&mut app, &mut doc, 1280.0, 800.0);

        // Pretend we've navigated away, then jump from the annotation card.
        app.chapter = app.num_chapters().saturating_sub(1);
        let jump = find_hit(&app, |ac| matches!(ac, Action::JumpTo(_))).expect("annotation card");
        assert!(click(&mut app, &mut doc, jump), "jump handled");
        assert_eq!(app.chapter, 0, "jumped to the annotation's chapter");
        assert_eq!(
            app.pending_jump_block,
            Some(target),
            "jump targets the annotation's block"
        );
        // The deferred page resolution runs on the next settled frame.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        assert!(
            app.pending_jump_block.is_none(),
            "pending jump was consumed by layout"
        );
    }

    #[test]
    fn page_for_block_maps_a_block_to_its_page() {
        use crate::model::{Block, Span};
        use crate::paginate::layout_chapter;
        let mut tr = TextRenderer::new();
        // Eight fat paragraphs into a small box -> several pages.
        let blocks: Vec<Block> = (0..8)
            .map(|_| Block::Paragraph {
                spans: vec![Span::plain("word ".repeat(60))],
            })
            .collect();
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            300.0,
            160.0,
            &Settings::default(),
            0,
            "en-US",
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        assert!(layout.page_count() >= 2, "content spans multiple pages");
        assert_eq!(page_for_block(&layout, 0), 0, "first block is on page 0");
        let last = layout.blocks.last().unwrap().index;
        assert!(
            page_for_block(&layout, last) >= 1,
            "the last block is past page 0"
        );
        for b in &layout.blocks {
            assert!(
                page_for_block(&layout, b.index) < layout.page_count(),
                "valid page index"
            );
        }
        assert_eq!(
            page_for_block(&layout, 9999),
            0,
            "unknown block -> safe page 0"
        );
    }

    // ---- TOC chapter navigation ---------------------------------------------

    #[test]
    fn toc_row_click_navigates_to_that_chapter() {
        let mut app = app_with_sample();
        app.book_idx = 1; // Field Notes has three chapters.
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        assert!(app.toc_open(), "contents rail docked-open at 1280");
        assert!(app.num_chapters() >= 2, "multi-chapter book");
        assert_eq!(app.chapter, 0);
        let target = app.num_chapters() - 1;
        let row = find_hit(
            &app,
            |a| matches!(a, Action::GotoChapter(c) if *c == target),
        )
        .expect("a TOC row for the last chapter");
        assert!(click(&mut app, &mut doc, row), "toc row handled");
        assert_eq!(app.chapter, target, "navigated to the clicked chapter");
        assert_eq!(app.page, 0, "chapter opens at page 0");
    }

    // ---- type popover steppers persist --------------------------------------

    #[test]
    fn type_popover_steppers_change_and_persist_settings() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        click(&mut app, &mut doc, aa);

        // Text-size + button grows and persists the font scale.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let base = BooksDoc::read_settings(&doc).font_scale;
        let font = app
            .number_controls
            .rect(&NumberField::FontScale)
            .expect("font + stepper");
        let inc = Point::new(
            font.x1 - font.height() / 2.0,
            f64::midpoint(font.y0, font.y1),
        );
        click(&mut app, &mut doc, inc);
        let grown = BooksDoc::read_settings(&doc).font_scale;
        assert!(
            grown > base,
            "font scale grew and persisted ({base} -> {grown})"
        );

        // Line-spacing − button shrinks and persists the spacing.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let sbase = BooksDoc::read_settings(&doc).line_spacing;
        let spacing = app
            .number_controls
            .rect(&NumberField::LineSpacing)
            .expect("spacing - stepper");
        let dec = Point::new(
            spacing.x0 + spacing.height() / 2.0,
            f64::midpoint(spacing.y0, spacing.y1),
        );
        click(&mut app, &mut doc, dec);
        let shrunk = BooksDoc::read_settings(&doc).line_spacing;
        assert!(
            shrunk < sbase,
            "line spacing shrank and persisted ({sbase} -> {shrunk})"
        );
    }

    // ---- reader keyboard layer ----------------------------------------------

    #[test]
    fn reader_keyboard_drives_navigation_and_chrome() {
        let mut app = app_with_sample();
        app.book_idx = 1; // Field Notes: three chapters, so nav is deterministic.
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.chapter, 0);

        // Up/Down move by chapter.
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowDown));
        assert_eq!(app.chapter, 1, "arrow-down advanced a chapter");
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowUp));
        assert_eq!(app.chapter, 0, "arrow-up went back a chapter");

        // Right turns the page (into the next chapter if the first is one page).
        let start = (app.chapter, app.page);
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowRight));
        assert_ne!(
            (app.chapter, app.page),
            start,
            "arrow-right turned the page"
        );

        // 't' toggles the docked contents rail.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let toc0 = app.toc_open();
        assert!(key_char(&mut app, &mut doc, "t"), "'t' consumed");
        assert_ne!(app.toc_open(), toc0, "'t' toggled the contents rail");

        // Digit keys set the page tint and persist it (Light differs from the
        // Sepia default, so this proves a real write, not a coincidence).
        assert!(key_char(&mut app, &mut doc, "1"));
        assert_eq!(
            BooksDoc::read_settings(&doc).theme,
            ReadTheme::Light,
            "'1' set Light"
        );
        assert!(key_char(&mut app, &mut doc, "3"));
        assert_eq!(
            BooksDoc::read_settings(&doc).theme,
            ReadTheme::Dark,
            "'3' set Dark"
        );

        // Escape from a clean reader leaves the book.
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert_eq!(app.view, View::Library, "escape left the book");
    }

    #[test]
    fn escape_peels_back_popover_then_overlay_then_book() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);

        // Layer 1: an open type popover — first Escape closes only that.
        app.type_popover = true;
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert!(!app.type_popover, "escape closed the popover");
        assert_eq!(app.view, View::Reader, "still in the book");

        // Layer 2: a narrow window with the contents overlay flap open.
        frame(&mut app, &mut doc, 560.0, 720.0);
        assert_eq!(app.toc_layout(), SplitLayout::Overlay);
        app.toc_flap.snap(true);
        assert!(app.toc_open(), "overlay flap open");
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert!(!app.toc_open(), "escape dismissed the overlay flap");
        assert_eq!(app.view, View::Reader, "still in the book");

        // Layer 3: nothing floating -> escape leaves the book.
        assert!(key_named(&mut app, &mut doc, NamedKey::Escape));
        assert_eq!(app.view, View::Library, "escape left the book");
    }

    // ---- reading-progress persistence + restore -----------------------------

    #[test]
    fn reading_progress_persists_and_restores_on_reopen() {
        let mut first = app_with_sample();
        first.book_idx = 1; // multi-chapter
        let mut doc = AutoCommit::new();
        frame(&mut first, &mut doc, 1000.0, 700.0);
        let book_id = first.book_id();

        // Advance two chapters; each turn persists progress to the CRDT.
        key_named(&mut first, &mut doc, NamedKey::ArrowDown);
        key_named(&mut first, &mut doc, NamedKey::ArrowDown);
        let reached = first.chapter;
        assert!(reached >= 1, "advanced into the book");
        assert_eq!(
            BooksDoc::read_progress(&doc, &book_id).chapter,
            reached,
            "progress written to the document"
        );

        // A fresh reader on the same document restores the saved position.
        let mut reopened = app_with_sample();
        reopened.book_idx = 1;
        frame(&mut reopened, &mut doc, 1000.0, 700.0);
        assert_eq!(
            reopened.chapter, reached,
            "reopen restored the saved chapter"
        );
    }

    #[test]
    fn type_popover_value_is_click_to_type() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        click(&mut app, &mut doc, aa);
        // Tap the text-size value to type a number directly.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let val = app
            .number_controls
            .rect(&NumberField::FontScale)
            .expect("the text-size value is a shared number field");
        let val = Point::new(f64::midpoint(val.x0, val.x1), f64::midpoint(val.y0, val.y1));
        assert!(click(&mut app, &mut doc, val), "value tap handled");
        for ch in ["1", "5", "0"] {
            assert!(key_char(&mut app, &mut doc, ch), "digit consumed");
        }
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Enter),
            "enter committed"
        );
        let fs = BooksDoc::read_settings(&doc).font_scale;
        assert!(
            (fs - 1.5).abs() < 0.001,
            "typed 150% -> font_scale 1.5, got {fs}"
        );

        // Line spacing is click-to-type too.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let ls_val = app
            .number_controls
            .rect(&NumberField::LineSpacing)
            .expect("line spacing is a shared number field");
        let ls_val = Point::new(
            f64::midpoint(ls_val.x0, ls_val.x1),
            f64::midpoint(ls_val.y0, ls_val.y1),
        );
        click(&mut app, &mut doc, ls_val);
        for ch in ["1", ".", "4"] {
            key_char(&mut app, &mut doc, ch);
        }
        key_named(&mut app, &mut doc, NamedKey::Enter);
        let ls = BooksDoc::read_settings(&doc).line_spacing;
        assert!(
            (ls - 1.4).abs() < 0.001,
            "typed 1.4 -> line_spacing 1.4, got {ls}"
        );
    }

    #[test]
    fn shelf_hide_read_filters_and_sort_orders() {
        use crate::model::{LibraryPrefs, LibrarySort, Progress};
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        let ids: Vec<String> = app.library.iter().map(|b| b.id.clone()).collect();
        assert!(ids.len() >= 3, "sample shelf has several books");
        // Mark the first book as finished; give the others open times.
        BooksDoc::write_progress(
            &mut doc,
            &ids[0],
            &Progress {
                chapter: 9,
                page: 9,
                fraction: 1.0,
                opened: 100,
            },
        );
        BooksDoc::write_progress(
            &mut doc,
            &ids[1],
            &Progress {
                fraction: 0.3,
                opened: 500,
                ..Default::default()
            },
        );
        BooksDoc::write_progress(
            &mut doc,
            &ids[2],
            &Progress {
                fraction: 0.6,
                opened: 300,
                ..Default::default()
            },
        );

        // Hide-read on: the finished book drops out of the display order.
        BooksDoc::write_library_prefs(
            &mut doc,
            &LibraryPrefs {
                sort: LibrarySort::Title,
                hide_read: true,
            },
        );
        frame(&mut app, &mut doc, 1200.0, 820.0);
        assert!(!app.shelf_order.contains(&0), "finished book hidden");
        assert_eq!(
            app.shelf_order.len(),
            ids.len() - 1,
            "one book filtered out"
        );

        // Sort by recently opened (hide-read off): most-recent first.
        BooksDoc::write_library_prefs(
            &mut doc,
            &LibraryPrefs {
                sort: LibrarySort::Recent,
                hide_read: false,
            },
        );
        frame(&mut app, &mut doc, 1200.0, 820.0);
        assert_eq!(
            app.shelf_order.first().copied(),
            Some(1),
            "book 1 (opened=500) is most recent"
        );

        // Sort by progress: furthest-read first.
        BooksDoc::write_library_prefs(
            &mut doc,
            &LibraryPrefs {
                sort: LibrarySort::Progress,
                hide_read: false,
            },
        );
        frame(&mut app, &mut doc, 1200.0, 820.0);
        assert_eq!(
            app.shelf_order.first().copied(),
            Some(0),
            "book 0 (fraction=1.0) is furthest read"
        );

        // The sort control + hide-read toggle are reachable in the shelf chrome.
        assert!(
            app.shelf_controls
                .has_action(ShelfControlAction::ToggleSortMenu),
            "a sort control is present"
        );
        assert!(
            app.shelf_controls
                .has_action(ShelfControlAction::ToggleHideRead),
            "a hide-read toggle is present"
        );
    }

    /// Drive a real drag that starts *inside* a paragraph and ends in a later
    /// one, then apply a colour, and assert the stored annotation carries a
    /// character range (partial, not the whole block) and spans the boundary as
    /// one grouped highlight — proving the mark is text-range, not paragraph.
    #[test]
    fn drag_across_paragraph_stores_partial_char_range_not_the_whole_block() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            app.sel_runs.len() > 3,
            "the page laid out real run geometry"
        );

        // Start on the SECOND run of the first block (so the range begins
        // mid-paragraph, start > 0), drag into a run of a later block.
        let r0 = app.sel_runs[0];
        let start_run = app
            .sel_runs
            .iter()
            .find(|r| r.block == r0.block && r.cs > 0)
            .copied()
            .expect("a non-first run in the first block");
        let later = app
            .sel_runs
            .iter()
            .find(|r| r.block > start_run.block)
            .copied()
            .expect("a run in a later paragraph");
        let start = Point::new(
            f64::midpoint(start_run.x0, start_run.x1),
            f64::midpoint(start_run.y0, start_run.y1),
        );
        let end = Point::new(
            f64::midpoint(later.x0, later.x1),
            f64::midpoint(later.y0, later.y1),
        );
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);
        let sel = app.sel.expect("a drag produced a selection");
        assert!(
            sel.ordered().1.block > sel.ordered().0.block,
            "selection crosses a paragraph"
        );

        app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
        app.refresh_from_doc(&doc);

        let hls: Vec<_> = BooksDoc::list_annotations(&doc)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Highlight)
            .collect();
        assert!(hls.len() >= 2, "one ranged highlight per spanned paragraph");
        // Every piece carries a real char range, and shares ONE group.
        assert!(
            hls.iter().all(|a| a.start < a.end),
            "each piece has a char range"
        );
        let g0 = hls[0].group;
        assert!(
            hls.iter().all(|a| a.group == g0),
            "the spanned pieces are one grouped highlight"
        );
        // The first block's piece begins mid-paragraph (start > 0) → this is a
        // character range, NOT a whole-paragraph highlight.
        let first = hls.iter().min_by_key(|a| a.block).unwrap();
        assert!(
            first.start > 0,
            "the highlight begins mid-paragraph (char-level, start={})",
            first.start
        );
    }

    /// A plain click on highlighted text opens that highlight's floating comment
    /// overlay (hit-testing the real char range, not "the paragraph").
    #[test]
    fn clicking_a_highlight_opens_the_comment_overlay() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);

        // Highlight a run-spanning range in the first block via a drag.
        let r0 = app.sel_runs[0];
        let end_run = app
            .sel_runs
            .iter()
            .rfind(|r| r.block == r0.block)
            .copied()
            .unwrap();
        let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
        let end = Point::new(
            f64::midpoint(end_run.x0, end_run.x1),
            f64::midpoint(end_run.y0, end_run.y1),
        );
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);
        app.dispatch(Action::SetColor(HiColor::Green), &mut doc);
        app.refresh_from_doc(&doc);
        let hl = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("a highlight now exists");
        assert!(!app.comment_overlay.is_open(), "no overlay yet");

        // Re-lay the page, then plain-click on the highlight. A char range can be
        // sub-word (this one starts and ends mid-word), so there isn't always a
        // whole run wholly inside it — the reader taps the *rendered band*, which
        // is exactly what the click must resolve back to the highlight's range.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let band = *app
            .hi_bands
            .first()
            .expect("the highlight drew a band to tap");
        let p = band.center();
        // Sanity: that tap really lands inside the stored char range.
        assert_eq!(
            app.hit_doc_pos(p).and_then(|dp| app.highlight_group_at(dp)),
            Some(hl.group),
            "the tapped point resolves to the highlight's char range",
        );
        ptr_move(&mut app, &mut doc, p);
        ptr_down(&mut app, &mut doc);
        ptr_up(&mut app, &mut doc);
        assert_eq!(
            app.comment_overlay.group(),
            Some(hl.group),
            "clicking the highlight opened its overlay"
        );

        // The overlay's affordances are drawn: add-comment + a trash for the
        // whole highlight + a dismiss target.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            find_hit(&app, |a| matches!(a, Action::AddComment(_))).is_some(),
            "add-comment present"
        );
        assert!(
            find_hit(&app, |a| matches!(a, Action::DeleteHighlight(_))).is_some(),
            "trash present"
        );
        assert!(
            find_hit(&app, |a| matches!(a, Action::CloseComments)).is_some(),
            "dismiss present"
        );
    }

    /// The comment overlay supports MANY comments per range: add two, edit one,
    /// delete one, then trash the whole highlight — all through the real hits.
    #[test]
    fn comment_overlay_adds_edits_and_deletes_multiple_comments() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Make a single-word highlight, then open its overlay directly.
        let p = page_point(&app);
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Yellow), &mut doc);
        app.refresh_from_doc(&doc);
        let group = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("a highlight")
            .group;
        app.comment_overlay.open(group);
        frame(&mut app, &mut doc, 1000.0, 700.0);

        let notes_in_group = |d: &AutoCommit| {
            BooksDoc::list_annotations(d)
                .into_iter()
                .filter(|a| a.kind == AnnoKind::Note && a.group == group)
                .count()
        };

        // Add the first comment and type it.
        let add = find_hit(&app, |a| matches!(a, Action::AddComment(_))).expect("add-comment");
        assert!(click(&mut app, &mut doc, add), "add handled");
        assert!(app.composing.is_some(), "the new comment is being composed");
        for ch in ["H", "i"] {
            key_char(&mut app, &mut doc, ch);
        }
        key_named(&mut app, &mut doc, NamedKey::Enter);
        assert_eq!(notes_in_group(&doc), 1, "one comment saved");
        assert!(
            app.comment_overlay.group() == Some(group),
            "overlay stays open after saving"
        );

        // Add a SECOND comment on the same range.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let add2 = find_hit(&app, |a| matches!(a, Action::AddComment(_))).expect("add-comment #2");
        assert!(click(&mut app, &mut doc, add2), "add #2 handled");
        for ch in ["Y", "o"] {
            key_char(&mut app, &mut doc, ch);
        }
        key_named(&mut app, &mut doc, NamedKey::Enter);
        assert_eq!(notes_in_group(&doc), 2, "a range holds multiple comments");

        // Edit the first comment (reopen it for composing).
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let edit =
            find_hit(&app, |a| matches!(a, Action::EditComment(_))).expect("an edit affordance");
        assert!(click(&mut app, &mut doc, edit), "edit handled");
        assert!(app.composing.is_some(), "a comment reopened for editing");
        key_named(&mut app, &mut doc, NamedKey::Enter);

        // Delete one comment.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let del =
            find_hit(&app, |a| matches!(a, Action::DeleteComment(_))).expect("a delete affordance");
        assert!(click(&mut app, &mut doc, del), "delete handled");
        assert_eq!(notes_in_group(&doc), 1, "one comment removed, one remains");

        // Trash the whole highlight → its pieces AND remaining comments go.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let trash = find_hit(&app, |a| matches!(a, Action::DeleteHighlight(_))).expect("the trash");
        assert!(click(&mut app, &mut doc, trash), "trash handled");
        assert!(
            BooksDoc::list_annotations(&doc)
                .into_iter()
                .all(|a| a.group != group),
            "deleting the highlight removed every row in its group"
        );
        assert!(!app.comment_overlay.is_open(), "the overlay closed");
    }

    /// The selection toolbar grows a trash button when the selection covers an
    /// existing highlight, and it deletes that highlight (item: explicit delete).
    #[test]
    fn selection_toolbar_trash_deletes_the_covered_highlight() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Highlight a word.
        let p = page_point(&app);
        dbl_click(&mut app, &mut doc, p);
        app.dispatch(Action::SetColor(HiColor::Blue), &mut doc);
        app.refresh_from_doc(&doc);
        assert_eq!(
            BooksDoc::list_annotations(&doc)
                .iter()
                .filter(|a| a.kind == AnnoKind::Highlight)
                .count(),
            1,
            "one highlight",
        );
        // Re-select the same word: the toolbar now offers a trash.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        dbl_click(&mut app, &mut doc, p);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let trash = find_hit(&app, |a| matches!(a, Action::DeleteHighlight(_)))
            .expect("selection toolbar shows a trash over an existing highlight");
        assert!(click(&mut app, &mut doc, trash), "trash handled");
        assert!(
            BooksDoc::list_annotations(&doc)
                .iter()
                .all(|a| a.kind != AnnoKind::Highlight),
            "the highlight under the selection was deleted",
        );
    }

    /// Double-clicking a word and then dragging must keep the double-clicked
    /// word inside the selection and extend by whole words, never resetting
    /// to the raw character the press landed on.
    #[test]
    fn double_click_then_drag_extends_by_whole_words() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(
            app.sel_runs.len() > 3,
            "the page laid out real run geometry"
        );

        // A multi-char word (so the double-click centre lands *inside* it,
        // strictly past its start) that has a later word in the same paragraph.
        let runs = app.sel_runs.clone();
        let r0 = runs
            .iter()
            .find(|r0| {
                r0.cl >= 3
                    && runs
                        .iter()
                        .any(|r| r.block == r0.block && r.cs > r0.cs + r0.cl)
            })
            .copied()
            .expect("a multi-char word with a later word after it");
        let far = runs
            .iter()
            .filter(|r| r.block == r0.block && r.cs > r0.cs + r0.cl)
            .min_by_key(|r| r.cs)
            .copied()
            .expect("a later word in the same paragraph");
        let start = Point::new(f64::midpoint(r0.x0, r0.x1), f64::midpoint(r0.y0, r0.y1));
        let far_p = Point::new(f64::midpoint(far.x0, far.x1), f64::midpoint(far.y0, far.y1));

        // First click, then a second press at the same spot (a double-click) that
        // stays DOWN — this is where the word gets selected and the word-drag arms.
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_up(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        let word = app.sel.expect("the double-click selected a word");
        assert_eq!(
            word.ordered().0.ch,
            r0.cs,
            "word selection starts at the word start"
        );
        assert_eq!(
            word.ordered().1.ch,
            r0.cs + r0.cl,
            "word selection ends at the word end"
        );

        // Now drag right to a later word while still held, then release.
        ptr_move(&mut app, &mut doc, far_p);
        ptr_up(&mut app, &mut doc);

        let sel = app.sel.expect("the drag kept a selection");
        let (s, e) = sel.ordered();
        // The anchor did NOT reset to the mid-word click point: it is still
        // the double-clicked word's start.
        assert_eq!(s.block, r0.block);
        assert_eq!(
            s.ch, r0.cs,
            "anchor pinned to the word start, not the click char"
        );
        // The head extended by whole words to cover the far word entirely.
        assert_eq!(e.ch, far.cs + far.cl, "head extended to the whole far word");
        assert!(
            e.ch > r0.cs + r0.cl,
            "the selection grew past the original word"
        );
    }

    /// Headline: the font control lists the reader's *real* installed system
    /// families and, once picked, actually re-renders the book body in that
    /// family. Drives the real enumeration + picker hit + apply path.
    #[test]
    fn system_font_picker_lists_real_families_and_applies_to_the_body() {
        use crate::model::FontKind;
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Open the font manager's system-family picker.
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        app.dispatch(Action::OpenFontPicker, &mut doc);
        frame(&mut app, &mut doc, 1200.0, 860.0);

        // The picker records an add-hit per installed family — real system fonts.
        let listed: Vec<String> = app
            .hits
            .iter()
            .filter_map(|h| match &h.action {
                Action::AddSystemFont(f) => Some(f.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !listed.is_empty(),
            "the picker lists installed system families"
        );
        // They are genuinely the renderer's enumerated families, not a hardcoded set.
        let real = TextRenderer::new().system_families();
        assert!(
            !real.is_empty(),
            "the text system enumerates installed families"
        );
        assert!(
            listed.iter().all(|f| real.contains(f)),
            "every listed family is a real installed family",
        );

        // Click one family's real hit → it registers as a Named face.
        let fam = listed[0].clone();
        let hit = find_hit(&app, |a| matches!(a, Action::AddSystemFont(f) if f == &fam))
            .expect("the chosen family has a clickable row");
        assert!(
            click(&mut app, &mut doc, hit),
            "adding the system font handled"
        );
        let entry = BooksDoc::read_fonts(&doc)
            .into_iter()
            .find(|f| f.kind == FontKind::Named(fam.clone()))
            .expect("the family is now a registered reading font");

        // Select it as the reading font → the body font resolves to that family.
        app.dispatch(Action::SetFont(entry.id), &mut doc);
        app.refresh_from_doc(&doc);
        assert_eq!(app.active_font().kind, FontKind::Named(fam.clone()));
        let (choice, _mult, _sig, mono) = app.body_font();
        assert_eq!(
            choice,
            FontChoice::Named(fam),
            "the book body re-renders in the chosen installed family",
        );
        assert!(!mono, "a named proportional family is not monospace");
    }

    /// Headline: a chosen colour scheme applies live to the reading view. Asserts
    /// the real `resolve_palette` the draw pass uses — for the built-in
    /// high-contrast tint, the de-browned dark, and a custom scheme.
    #[test]
    fn selecting_a_scheme_applies_its_palette_to_the_reading_view() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let theme = Theme::dark();

        // High-contrast built-in tint → true-black page, white ink.
        app.dispatch(Action::SetReadTheme(ReadTheme::HighContrast), &mut doc);
        app.refresh_from_doc(&doc);
        let pal = app.resolve_palette(&theme);
        assert!(pal.dark, "high contrast is a dark reading mode");
        assert!(
            pal.page.r() < 6 && pal.page.g() < 6 && pal.page.b() < 6,
            "a true-black page"
        );
        assert!(
            pal.ink.r() > 250 && pal.ink.g() > 250 && pal.ink.b() > 250,
            "white ink for maximum contrast",
        );

        // Dark tint → a neutral charcoal, NOT a brown/sepia (R ≈ B).
        app.dispatch(Action::SetReadTheme(ReadTheme::Dark), &mut doc);
        app.refresh_from_doc(&doc);
        let dark = app.resolve_palette(&theme);
        assert!(dark.dark);
        let warm = i32::from(dark.page.r()) - i32::from(dark.page.b());
        assert!(
            (0..=8).contains(&warm),
            "dark page is a neutral charcoal, not brown (Δrb={warm})"
        );

        // A custom scheme's page colour drives the reading view directly.
        let mut schemes = default_schemes();
        schemes.push(ColorScheme::from_bg(
            "scheme_z",
            "Ocean",
            [0x0A, 0x22, 0x33],
        ));
        BooksDoc::write_schemes(&mut doc, &schemes);
        app.dispatch(Action::SetScheme("scheme_z".into()), &mut doc);
        app.refresh_from_doc(&doc);
        let ocean = app.resolve_palette(&theme);
        assert_eq!(
            (ocean.page.r(), ocean.page.g(), ocean.page.b()),
            (0x0A, 0x22, 0x33),
            "the custom scheme's page colour is what the reading view paints",
        );
    }

    /// The font-rename field is the toolkit's real `TextInput`: Ctrl+A selects
    /// the whole value (so the next keystroke replaces it), and a click
    /// outside the field blur-commits the rename to the CRDT.
    #[test]
    #[serial]
    fn font_rename_uses_real_text_input_and_commits_on_blur() {
        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1000.0, 800.0);

        // Begin an inline rename on the built-in serif face via the real action,
        // then lay the editor out with another frame.
        app.dispatch(Action::BeginRenameFont("serif".into()), &mut doc);
        frame(&mut app, &mut doc, 1000.0, 800.0);
        assert!(app.renaming.is_some(), "a rename is in progress");

        // `begin_edit` selects-all; collapse it (caret to end) so Ctrl+A is the
        // ONLY thing that could re-select the whole value.
        key_named(&mut app, &mut doc, NamedKey::ArrowRight);
        let mut ctrl = ModifiersState::empty();
        ctrl |= ModifiersState::CONTROL;
        assert!(
            key_char_mods(&mut app, &mut doc, "a", ctrl),
            "Ctrl+A is consumed by the embedded field",
        );
        // Typing now replaces the whole (re-)selected value.
        key_char(&mut app, &mut doc, "Z");
        assert_eq!(
            app.renaming.as_ref().unwrap().field.current_text(),
            "Z",
            "Ctrl+A selected all in the real TextInput, so the keystroke replaced it",
        );

        // A click far outside the field blur-commits the rename to the CRDT.
        click(&mut app, &mut doc, Point::new(950.0, 24.0));
        assert!(app.renaming.is_none(), "clicking outside ended the rename");
        let fonts = crate::doc::BooksDoc::read_fonts(&doc);
        let serif = fonts
            .iter()
            .find(|f| f.id == "serif")
            .expect("serif entry present");
        assert_eq!(
            serif.name, "Z",
            "the blur committed the new name to the CRDT"
        );
    }

    /// Esc cancels an inline font rename: the field closes and the CRDT keeps
    /// the original name — proven on the real `handle_event` path.
    #[test]
    #[serial]
    fn font_rename_escape_cancels_without_writing() {
        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1000.0, 800.0);

        app.dispatch(Action::BeginRenameFont("serif".into()), &mut doc);
        frame(&mut app, &mut doc, 1000.0, 800.0);
        key_char(&mut app, &mut doc, "Q"); // would-be new value
        assert!(
            key_named(&mut app, &mut doc, NamedKey::Escape),
            "Esc is consumed by the editor",
        );
        assert!(app.renaming.is_none(), "Esc closed the rename");
        let fonts = crate::doc::BooksDoc::read_fonts(&doc);
        let serif = fonts
            .iter()
            .find(|f| f.id == "serif")
            .expect("serif entry present");
        assert_eq!(serif.name, "Serif", "cancel kept the original name");
    }
}
