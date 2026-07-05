//! The Books app widget — a single retained-mode [`Widget`] that renders both
//! the **library shelf** and the **paginated reader**, sourcing every colour
//! from the OS [`Theme`] (chrome) or a reader palette (the page), and storing
//! every piece of user state in the Automerge CRDT.
//!
//! ## Why one custom widget rather than composed `ui` widgets
//!
//! An e-reader page is reflowable, paginated, measurement-bound text with
//! highlight bands painted *behind* glyphs and bookmark/note glyphs in the
//! margins — none of which maps onto the stock layout widgets. So the whole
//! surface is drawn straight into the Vello [`Scene`] using the engine in
//! [`crate::paginate`] and the vector helpers in [`crate::paint`]. Interaction
//! uses the classic immediate-mode contract: `draw` records clickable [`Hit`]
//! regions; `handle_event` hit-tests them on the next click. A frame is always
//! drawn before input is processed, so the regions are never stale.
//!
//! ## Where state lives
//!
//! - **CRDT** (`State::doc`): reading settings, per-book progress, and every
//!   annotation. Mutated directly in `handle_event` / `sync`.
//! - **Ephemeral widget caches**: which book is open, the live page index, the
//!   selected block, the layout cache. These are per-peer view state and stay
//!   out of the document so two readers can browse independently.

use std::collections::HashMap;

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
use winit::keyboard::{Key, NamedKey};

use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};
use ui::ColorCompat;

use crate::content::load_library;
use crate::doc::BooksDoc;
use crate::model::{
    default_fonts, default_schemes, now_ms, AnnoKind, Annotation, Book, ColorScheme, FontEntry,
    FontKind, HiColor, Progress, ReadTheme, Settings,
};
use crate::paginate::{font_choice, layout_chapter, ChapterLayout, LayoutKey, LayoutKind};
use crate::paint;
use crate::reader_theme::ReaderPalette;

const TOP_BAR: f64 = 54.0;
const BOTTOM_BAR: f64 = 58.0;

/// Pixels one wheel-line notch maps to, matching the toolkit's other scroll
/// surfaces so a mouse wheel feels consistent across apps.
const WHEEL_LINE_PX: f64 = 48.0;
/// Accumulated reader wheel travel (px) that commits to one page turn.
const TURN_STEP_PX: f64 = 40.0;
/// Accumulated horizontal swipe travel (px) that commits to one page turn. A
/// touch/trackpad swipe should be a deliberate flick, so the threshold is
/// larger than the vertical wheel step.
const SWIPE_STEP_PX: f64 = 80.0;
const TOC_W: f64 = 244.0;
const NOTES_W: f64 = 326.0;
const PAGE_MAX_W: f64 = 680.0;

/// At/above this allocated width the reader draws the floating "paper card"
/// (shadow, rounded edge, margin). Below it, the page goes borderless and fills
/// the whole reading area — on a small screen the book is the focus, and a
/// framed card would just waste space around the text.
const FRAME_MIN_W: f64 = 720.0;

/// Below this allocated width (logical px) the **contents** rail (the primary
/// navigation sidebar) switches from docked-beside-the-page to an overlay flap
/// that slides over the page behind a dimming scrim. Chosen so the rail + a
/// readable page column stop fitting comfortably side by side.
const TOC_BREAKPOINT: f64 = 760.0;

/// Below this (wider) width the **annotations** rail also goes overlay. Set
/// higher than the contents breakpoint so a medium window keeps the contents
/// docked but floats the annotations panel rather than crushing the page.
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
}

/// An in-progress inline rename (font display name or scheme name).
#[derive(Clone)]
struct Rename {
    target: RenameTarget,
    buf: String,
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
#[derive(Clone, Copy, PartialEq, Eq)]
struct DocPos {
    block: usize,
    ch: usize,
}

impl DocPos {
    fn key(&self) -> (usize, usize) {
        (self.block, self.ch)
    }
}

/// A drag text selection: an anchor (where the drag started) and a head (where
/// it is now). Ordered on demand; spans paragraphs and, as the reader turns
/// pages, renders on whichever page shows part of the range.
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
/// (which has no text renderer) can map a pointer to a [`DocPos`], and so the
/// selection bands can be measured once. Rebuilt every `draw`.
#[derive(Clone, Copy)]
struct SelRun {
    block: usize,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    cs: usize,
    cl: usize,
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
    /// Toggle the reading-settings (type + page) popover above the bottom bar.
    ToggleTypePopover,
    /// Dismiss any open overlay flap (scrim tap on a narrow window).
    DismissOverlay,
    /// Absorb a click without acting — used to stop a tap on an overlay rail's
    /// own body from falling through to the dismissing scrim behind it.
    Swallow,
    ToggleBookmark,
    FontInc,
    FontDec,
    SpacingInc,
    SpacingDec,
    CycleFont,
    /// Select a registered font by id (quick-settings font pill).
    SetFont(String),
    SetReadTheme(ReadTheme),
    SetColor(HiColor),
    SelectBlock(usize),
    AddNote,
    /// Finish composing the current on-page note.
    FinishNote,
    JumpTo(Uuid),
    Delete(Uuid),
    EditNote(Uuid),
    /// Re-scan the on-disk library directories (the empty-shelf affordance —
    /// picks up `.epub` files added to the Books folder without a restart).
    ReloadLibrary,

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
    /// Advance one colour slot (0=page,1=ink,2=accent) of a scheme through the
    /// editor's colour ramp.
    CycleSchemeColor(String, usize),
    /// Create a new scheme derived from a single background colour.
    NewSchemeFromBg([u8; 3]),
    BeginRenameScheme(String),

    // --- settings screen: fonts ------------------------------------------
    SetDefaultFont(String),
    ToggleFontShown(String),
    FontMultInc(String),
    FontMultDec(String),
    RemoveFont(String),
    OpenFontPicker,
    CloseFontPicker,
    AddSystemFont(String),
    BeginRenameFont(String),
}

/// Icon glyphs the chrome draws (each maps to a `paint::*` vector helper).
#[derive(Clone, Copy)]
enum Ico {
    Back,
    Toc,
    Notes,
    Bookmark(bool),
    Prev,
    Next,
    PlusI,
    MinusI,
}

pub struct BooksApp {
    // --- parsed content (render cache, regenerable; not in the CRDT) -------
    library: Vec<Book>,

    // --- taffy / paint geometry -------------------------------------------
    rect: Rect,
    dirty: bool,

    // --- view state (ephemeral, per-peer) ---------------------------------
    view: View,
    book_idx: usize,
    chapter: usize,
    page: usize,
    /// Contents rail — the primary navigation sidebar. Docked beside the page
    /// on a wide window, an overlay flap (closed by default, summoned by the
    /// hamburger) on a narrow one. The shared toolkit kernel owns the eased
    /// reveal + scrim so the app never hand-rolls collapse logic.
    toc_flap: FlapState,
    /// Annotations rail — a secondary inspector. Summoned on demand even when
    /// docked, and floats as an overlay flap below [`NOTES_BREAKPOINT`].
    notes_flap: FlapState,
    selected_block: Option<usize>,
    /// Arbitrary drag text selection spanning paragraphs/pages, if any.
    sel: Option<TextSel>,
    /// Whether the left button is currently down (for drag selection).
    mouse_down: bool,
    /// Screen point where the current press began.
    press_pos: Point,
    /// Doc position under the press, if it landed on page text.
    press_doc: Option<DocPos>,
    /// Whether the current press has moved far enough to be a selection drag.
    dragging_sel: bool,
    /// Per-frame run geometry for pointer→DocPos hit-testing (rebuilt in draw).
    sel_runs: Vec<SelRun>,
    /// After a drag auto-advances a page, extend the selection head to the new
    /// page's end (`Some(true)`) or start (`Some(false)`) once it's laid out.
    pending_sel_edge: Option<bool>,
    /// The reader body's vertical extent (top, bottom) for edge auto-advance.
    reader_body: (f64, f64),
    /// First / last doc position visible on the current page (for edge grow).
    page_first_doc: Option<DocPos>,
    page_last_doc: Option<DocPos>,
    hi_color: HiColor,
    /// Vertical scroll offset of the library shelf (logical px). The shelf can
    /// hold far more books than fit on screen, so it scrolls; this is the
    /// current offset, clamped each frame to `shelf_max_scroll`.
    shelf_scroll: f64,
    /// Last-known maximum shelf scroll, computed during `draw_library` where the
    /// column count + row heights are known, and read back by the wheel handler
    /// (which runs before the next layout) to clamp the offset.
    shelf_max_scroll: f64,
    /// Keyboard-focused book on the shelf. Arrow keys move it, Enter/Space
    /// opens it — so the library is reachable without a pointer.
    shelf_cursor: usize,
    /// Last-known column count of the shelf grid, computed in `draw_library` and
    /// read back by the keyboard handler so up/down move by a whole row.
    shelf_cols: usize,
    /// Set when the shelf cursor moves via the keyboard, so the next
    /// `draw_library` scrolls the focused card into view.
    scroll_to_cursor: bool,
    /// Accumulated wheel delta in the reader, so a trackpad's many small pixel
    /// deltas coalesce into whole page turns instead of flipping wildly.
    turn_accum: f64,
    /// Reading-settings popover (text size / spacing / typeface / page tint),
    /// summoned from the bottom bar. Keeps those controls out of the always-on
    /// chrome so the bar stays readable at every width.
    type_popover: bool,
    cursor: Point,
    /// First block index visible on the current page (for bookmark targeting).
    page_top_block: usize,
    /// The note currently being typed into (a CRDT annotation id).
    composing: Option<Uuid>,
    /// Deferred page resolution: jump to last page after a backward chapter
    /// turn, or to the page containing a block after a sidebar jump.
    pending_last_page: bool,
    pending_jump_block: Option<usize>,

    /// Accumulated horizontal swipe travel (px) for trackpad/touch page turns.
    swipe_accum: f64,

    // --- caches pulled from the CRDT (refreshed in sync / after events) ----
    settings: Settings,
    /// The registered fonts (built-ins + user-added), read from the CRDT.
    fonts: Vec<FontEntry>,
    /// Registry-wide default font id.
    default_font_id: String,
    /// The registered colour schemes (built-ins + user-created), from the CRDT.
    schemes: Vec<ColorScheme>,
    annotations: Vec<Annotation>,

    // --- settings screen (ephemeral) --------------------------------------
    settings_tab: SettingsTab,
    /// An in-progress inline rename, if any.
    renaming: Option<Rename>,
    /// Scheme id currently open in the colour editor, if any.
    editing_scheme: Option<String>,
    /// Whether the "add a system font" family picker is open.
    font_picker: bool,
    /// Scroll offset of the family picker list.
    picker_scroll: f64,
    picker_max_scroll: f64,
    /// Scroll offset of the settings screen body.
    settings_scroll: f64,
    settings_max_scroll: f64,
    shelf_fraction: HashMap<String, f32>,
    shelf_annos: HashMap<String, usize>,
    last_persisted: Option<(usize, usize)>,
    cur_page_count: usize,
    seeded: bool,
    /// Sample-content mode: only the hermetic `--shots`/test path sets this. It
    /// swaps in the multi-book sample library and seeds demo annotations so a
    /// screenshot shows a lived-in reader. A real user is never seeded.
    demo: bool,
    /// Reading settings a `--shots` scenario wants applied through the real CRDT
    /// path on the next sync (e.g. a serif font or the dark tint). Shot-only.
    shot_settings: Option<Settings>,
    /// Font/scheme registries a `--shots` scenario seeds so the managers show a
    /// lived-in registry (a custom font, a custom scheme). Shot-only.
    shot_fonts: Option<Vec<FontEntry>>,
    shot_schemes: Option<Vec<ColorScheme>>,
    /// A `--shots` scenario that wants the on-page note composer open: seed a
    /// note anchored to the staged selection on the next sync. Shot-only.
    shot_compose: bool,
    /// Last resolved layout mode per rail, so a breakpoint crossing (or the
    /// first real width) settles the flap instantly instead of animating a
    /// gratuitous slide on launch/resize.
    settled_toc: Option<SplitLayout>,
    settled_notes: Option<SplitLayout>,

    // --- layout cache ------------------------------------------------------
    layout: Option<ChapterLayout>,
    /// Decoded cover artwork lifted into GPU-ready image handles, keyed by book
    /// id and built lazily on first shelf draw (never per frame).
    cover_cache: HashMap<String, PenikoImage>,

    // --- per-frame hit regions --------------------------------------------
    hits: Vec<Hit>,
}

impl BooksApp {
    pub fn new() -> Self {
        Self::with_library(load_library())
    }

    fn with_library(library: Vec<Book>) -> Self {
        // Open straight into the reader when there is a book to read (the first
        // one on the shelf); otherwise land on the library so an empty shelf
        // shows its guidance rather than a blank page.
        let view = if library.is_empty() { View::Library } else { View::Reader };
        Self {
            library,
            rect: Rect::ZERO,
            dirty: true,
            view,
            book_idx: 0,
            chapter: 0,
            page: 0,
            toc_flap: FlapState::open_when_docked(),
            notes_flap: FlapState::open_when_docked(),
            selected_block: None,
            sel: None,
            mouse_down: false,
            press_pos: Point::ZERO,
            press_doc: None,
            dragging_sel: false,
            sel_runs: Vec::new(),
            pending_sel_edge: None,
            reader_body: (0.0, 0.0),
            page_first_doc: None,
            page_last_doc: None,
            hi_color: HiColor::Yellow,
            shelf_scroll: 0.0,
            shelf_max_scroll: 0.0,
            shelf_cursor: 0,
            shelf_cols: 1,
            scroll_to_cursor: false,
            turn_accum: 0.0,
            type_popover: false,
            cursor: Point::ZERO,
            page_top_block: 0,
            composing: None,
            pending_last_page: false,
            pending_jump_block: None,
            swipe_accum: 0.0,
            settings: Settings::default(),
            fonts: default_fonts(),
            default_font_id: String::new(),
            schemes: default_schemes(),
            annotations: Vec::new(),
            settings_tab: SettingsTab::Fonts,
            renaming: None,
            editing_scheme: None,
            font_picker: false,
            picker_scroll: 0.0,
            picker_max_scroll: 0.0,
            settings_scroll: 0.0,
            settings_max_scroll: 0.0,
            shelf_fraction: HashMap::new(),
            shelf_annos: HashMap::new(),
            last_persisted: None,
            cur_page_count: 1,
            seeded: false,
            demo: false,
            shot_settings: None,
            shot_fonts: None,
            shot_schemes: None,
            shot_compose: false,
            settled_toc: None,
            settled_notes: None,
            layout: None,
            cover_cache: HashMap::new(),
            hits: Vec::new(),
        }
    }

    /// Build the reader pointed at a single opened `.epub` (OS file
    /// association). Falls back to the normal library if the file can't be
    /// parsed, so a bad hand-off still opens something usable.
    pub fn open_path(path: &std::path::Path) -> Self {
        match crate::content::open_epub_path(path) {
            Some(book) => Self::with_library(vec![book]),
            None => Self::new(),
        }
    }

    fn num_chapters(&self) -> usize {
        self.library
            .get(self.book_idx)
            .map(|b| b.chapters.len())
            .unwrap_or(0)
    }

    // ======================================================================
    // Responsive layout — the contents / annotations rails collapse to
    // overlay flaps on a narrow window, driven entirely by the allocated
    // width against the breakpoints. The shared `FlapState` kernel owns the
    // eased reveal + scrim; the app only resolves which mode applies.
    // ======================================================================

    /// Resolve the contents rail's layout from the allocated width.
    fn toc_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), TOC_BREAKPOINT)
    }

    /// Resolve the annotations rail's layout from the allocated width.
    fn notes_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), NOTES_BREAKPOINT)
    }

    /// Whether the contents rail is currently open (preference vs. mode default).
    fn toc_open(&self) -> bool {
        self.toc_flap.is_open(self.toc_layout())
    }

    /// Whether the annotations rail is currently open.
    fn notes_open(&self) -> bool {
        self.notes_flap.is_open(self.notes_layout())
    }

    /// Reconcile both flaps with the current width. On the first real width and
    /// on every breakpoint crossing the flap is *settled* (snapped) to its mode
    /// default, so launch and resize never play a gratuitous slide — animation
    /// is reserved for explicit user toggles within one layout mode. Called
    /// each frame from `apply_layout`/`draw`, so a resize returns to auto.
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

    /// True while either rail's eased reveal is still settling toward its
    /// target — drives the host's "request another frame" decision.
    fn flaps_animating(&self) -> bool {
        self.toc_flap.is_animating(self.toc_layout())
            || self.notes_flap.is_animating(self.notes_layout())
    }

    /// Stage a named interaction state for the single-frame `--shots`
    /// renderer (which can't reach these states through live input). This is
    /// the ONLY place sample content enters the app: it swaps in the multi-book
    /// sample library and marks the app as demo so `sync` seeds demo
    /// annotations. Flaps are *snapped* (no animation) so the frame is settled.
    ///
    /// Recognised scenarios:
    /// * `library` — the shelf, showing several sample books.
    /// * `library-scrolled` — the shelf scrolled to the bottom (exercises the
    ///   scroll offset, top-bar clip, and indicator).
    /// * `narrow`  — the contents rail summoned as an overlay flap over the
    ///   page behind a scrim (the narrow-window layout).
    /// * `type`    — the reading-settings popover open above the bottom bar.
    /// * `select`  — a paragraph selected, floating the selection toolbar.
    /// * anything else — the reader on the first sample book (clean page).
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        // The shelf/read scenarios render the *real* disk-discovered library
        // (used to verify discovery + cover decoding + a clean first-open with
        // no fabricated marks), so they deliberately do not enter demo mode.
        if scenario == "shelf" {
            self.view = View::Library;
            return;
        }
        if scenario == "read" {
            self.view = View::Reader;
            return;
        }
        self.demo = true;
        self.library = crate::content::load_sample_library();
        self.book_idx = 0;
        self.chapter = 0;
        self.page = 0;
        match scenario {
            "library" => self.view = View::Library,
            "serif" => {
                // The reader with a serif body face applied — through the real
                // settings path, so it exercises serif shaping end to end.
                self.view = View::Reader;
                self.shot_settings = Some(Settings { font_id: "serif".into(), ..Settings::default() });
            }
            "dark" => {
                // The warm dark page tint.
                self.view = View::Reader;
                self.shot_settings =
                    Some(Settings { theme: ReadTheme::Dark, ..Settings::default() });
            }
            "settings-colours" => {
                // The colour-scheme manager with a custom scheme present.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Colours;
                let mut sc = default_schemes();
                sc[0].enabled = false; // a disabled built-in
                sc.push(ColorScheme::from_bg("scheme1", "Midnight", [0x15, 0x1A, 0x24]));
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
            "scheme-editor" => {
                // The per-colour scheme editor open on a custom scheme.
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Colours;
                let mut sc = default_schemes();
                sc.push(ColorScheme::from_bg("scheme1", "Midnight", [0x15, 0x1A, 0x24]));
                self.shot_schemes = Some(sc);
                self.editing_scheme = Some("scheme1".into());
            }
            "select" => {
                // A selected paragraph so the shot exercises the floating
                // selection toolbar (swatches + Note) over the page.
                self.view = View::Reader;
                self.selected_block = Some(2);
            }
            "select-drag" => {
                // An arbitrary cross-paragraph selection so the shot shows real
                // selection bands + the HUD. Non-demo so no seeded highlights
                // clutter the bands.
                self.demo = false;
                self.view = View::Reader;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 1, ch: 6 },
                    head: DocPos { block: 2, ch: 60 },
                });
                self.selected_block = Some(1);
            }
            "note" => {
                // A cross-paragraph selection with the on-page note composer open.
                self.demo = false;
                self.view = View::Reader;
                self.sel = Some(TextSel {
                    anchor: DocPos { block: 1, ch: 6 },
                    head: DocPos { block: 2, ch: 60 },
                });
                self.selected_block = Some(1);
                self.shot_compose = true;
            }
            "library-scrolled" => {
                // Scrolled to the bottom of an overflowing shelf so the shot
                // exercises the scroll offset, the clip under the top bar, and
                // the indicator. The large value clamps to the real max in draw.
                self.view = View::Library;
                self.shelf_scroll = 5000.0;
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
            _ => self.view = View::Reader,
        }
    }

    // ======================================================================
    // CRDT <-> cache plumbing
    // ======================================================================

    /// Seed the demo library's annotations so a `--shots` frame shows a
    /// lived-in reader (a bookmark, two highlights, and a typed note). This runs
    /// ONLY in demo mode — a real user's document is never populated with
    /// fabricated content; they start with a clean book and their own marks.
    fn seed_if_needed(&mut self, doc: &mut AutoCommit) {
        if self.seeded || !self.demo {
            return;
        }
        self.seeded = true;

        if !BooksDoc::list_annotations(doc).is_empty() {
            return;
        }
        let Some(book) = self.library.get(0) else {
            return;
        };
        let id = book.id.clone();
        let ch0 = &book.chapters[0];
        let excerpt = |i: usize| -> String {
            let t = ch0.block_text(i);
            let mut s: String = t.chars().take(96).collect();
            if t.chars().count() > 96 {
                s.push('…');
            }
            s
        };
        // A bookmark on the opening block.
        let bm = Annotation::new(&id, AnnoKind::Bookmark, 0, 0).with_excerpt(excerpt(0));
        BooksDoc::upsert_annotation(doc, &bm);
        // Two coloured highlights on early paragraphs.
        let h1 = Annotation::new(&id, AnnoKind::Highlight, 0, 1)
            .with_color(HiColor::Yellow)
            .with_excerpt(excerpt(1));
        BooksDoc::upsert_annotation(doc, &h1);
        let h2 = Annotation::new(&id, AnnoKind::Highlight, 0, 3)
            .with_color(HiColor::Green)
            .with_excerpt(excerpt(3));
        BooksDoc::upsert_annotation(doc, &h2);
        // A note with real, typed text.
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

        let ids: Vec<String> = self.library.iter().map(|b| b.id.clone()).collect();
        let cur_id = ids.get(self.book_idx).cloned().unwrap_or_default();
        self.annotations = BooksDoc::annotations_for(doc, &cur_id);

        let all = BooksDoc::list_annotations(doc);
        self.shelf_fraction.clear();
        self.shelf_annos.clear();
        for id in &ids {
            let p = BooksDoc::read_progress(doc, id);
            self.shelf_fraction.insert(id.clone(), p.fraction);
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
            },
        );
        self.last_persisted = Some((self.chapter, self.page));
    }

    // ======================================================================
    // Action dispatch (event side — has the document)
    // ======================================================================

    fn dispatch(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::OpenBook(i) => {
                self.book_idx = i.min(self.library.len().saturating_sub(1));
                self.view = View::Reader;
                let id = self.library[self.book_idx].id.clone();
                let p = BooksDoc::read_progress(doc, &id);
                let nchap = self.num_chapters().saturating_sub(1);
                self.chapter = p.chapter.min(nchap);
                self.page = p.page;
                self.selected_block = None;
                self.composing = None;
                self.layout = None;
                self.last_persisted = None;
            }
            Action::Back => {
                self.view = View::Library;
                self.composing = None;
                self.type_popover = false;
            }
            Action::NextPage => {
                let pc = self.cur_page_count.max(1);
                if self.page + 1 < pc {
                    self.page += 1;
                } else if self.chapter + 1 < self.num_chapters() {
                    self.chapter += 1;
                    self.page = 0;
                    self.selected_block = None;
                    self.layout = None;
                }
            }
            Action::PrevPage => {
                if self.page > 0 {
                    self.page -= 1;
                } else if self.chapter > 0 {
                    self.chapter -= 1;
                    self.selected_block = None;
                    self.pending_last_page = true;
                    self.layout = None;
                }
            }
            Action::GotoChapter(c) => {
                self.chapter = c.min(self.num_chapters().saturating_sub(1));
                self.page = 0;
                self.selected_block = None;
                self.layout = None;
            }
            Action::ToggleToc => {
                let layout = self.toc_layout();
                self.toc_flap.toggle(layout);
            }
            Action::ToggleNotes => {
                let layout = self.notes_layout();
                self.notes_flap.toggle(layout);
            }
            Action::ToggleTypePopover => {
                self.type_popover = !self.type_popover;
            }
            Action::DismissOverlay => {
                self.type_popover = false;
                if self.toc_layout() == SplitLayout::Overlay && self.toc_open() {
                    self.toc_flap.close();
                }
                if self.notes_layout() == SplitLayout::Overlay && self.notes_open() {
                    self.notes_flap.close();
                }
            }
            Action::Swallow => {}
            Action::ToggleBookmark => self.toggle_bookmark(doc),
            Action::FontInc => self.bump_setting(doc, |s| {
                s.font_scale = (s.font_scale + 0.1).clamp(Settings::FONT_MIN, Settings::FONT_MAX)
            }),
            Action::FontDec => self.bump_setting(doc, |s| {
                s.font_scale = (s.font_scale - 0.1).clamp(Settings::FONT_MIN, Settings::FONT_MAX)
            }),
            Action::SpacingInc => self.bump_setting(doc, |s| {
                s.line_spacing =
                    (s.line_spacing + 0.1).clamp(Settings::SPACING_MIN, Settings::SPACING_MAX)
            }),
            Action::SpacingDec => self.bump_setting(doc, |s| {
                s.line_spacing =
                    (s.line_spacing - 0.1).clamp(Settings::SPACING_MIN, Settings::SPACING_MAX)
            }),
            Action::CycleFont => {
                // Cycle through the fonts offered in quick settings.
                let shown = self.shown_fonts();
                if !shown.is_empty() {
                    let cur = self.active_font().id;
                    let idx = shown.iter().position(|f| f.id == cur).unwrap_or(0);
                    let next = shown[(idx + 1) % shown.len()].id.clone();
                    self.bump_setting(doc, |s| s.font_id = next);
                }
            }
            Action::SetFont(id) => {
                self.bump_setting(doc, |s| s.font_id = id);
            }
            Action::SetReadTheme(t) => self.bump_setting(doc, |s| {
                s.theme = t;
                // Selecting a built-in tint clears any custom scheme override.
                s.scheme_id = String::new();
            }),
            Action::SetColor(c) => {
                self.hi_color = c;
                // A drag selection highlights its exact range across paragraphs;
                // otherwise a selected block highlights whole.
                if let Some(sel) = self.sel.filter(|s| !s.is_empty()) {
                    self.add_highlight_range(doc, sel, c);
                    self.clear_selection();
                } else if let Some(b) = self.selected_block {
                    self.add_highlight(doc, b, c);
                }
            }
            Action::SelectBlock(b) => {
                self.selected_block = if self.selected_block == Some(b) {
                    None
                } else {
                    Some(b)
                };
            }
            Action::AddNote => {
                // Anchor a note to the selection (spanning paragraphs) or, if no
                // drag selection, to the selected block. The note is composed in
                // an on-page overlay near the passage, not a separate screen.
                let anchor = self
                    .sel
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        let (start, end) = s.ordered();
                        (start.block, start.ch, end.ch.max(start.ch))
                    })
                    .or_else(|| self.selected_block.map(|b| (b, 0, 0)));
                if let Some((block, start, end)) = anchor {
                    let id = self.book_id();
                    let excerpt = if self.sel.map_or(false, |s| !s.is_empty()) {
                        self.selection_excerpt()
                    } else {
                        self.block_excerpt(block)
                    };
                    let mut a = Annotation::new(&id, AnnoKind::Note, self.chapter, block)
                        .with_color(self.hi_color)
                        .with_excerpt(excerpt);
                    a.start = start;
                    a.end = end;
                    a.note = String::new();
                    let aid = a.id;
                    BooksDoc::upsert_annotation(doc, &a);
                    self.composing = Some(aid);
                }
            }
            Action::FinishNote => {
                self.edit_note_text(doc, NoteEdit::Done);
            }
            Action::EditNote(id) => {
                self.composing = Some(id);
                self.notes_flap.open();
            }
            Action::JumpTo(id) => {
                if let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() {
                    self.chapter = a.chapter.min(self.num_chapters().saturating_sub(1));
                    self.pending_jump_block = Some(a.block);
                    self.selected_block = Some(a.block);
                    self.layout = None;
                }
            }
            Action::Delete(id) => {
                if self.composing == Some(id) {
                    self.composing = None;
                }
                BooksDoc::remove_annotation(doc, id);
            }
            Action::ReloadLibrary => {
                // Re-scan the on-disk library directories. If the reader has
                // since dropped `.epub` files into their Books folder (as the
                // empty-shelf hint suggests), this picks them up live. Stays on
                // the shelf so the freshly discovered books are visible; only an
                // explicit tap on a card enters the reader.
                self.library = load_library();
                self.cover_cache.clear();
                self.book_idx = 0;
                self.shelf_cursor = 0;
                self.shelf_scroll = 0.0;
                self.scroll_to_cursor = false;
                self.layout = None;
            }

            // --- settings screen: navigation -----------------------------
            Action::OpenSettings => {
                self.view = View::Settings;
                self.type_popover = false;
                self.font_picker = false;
                self.editing_scheme = None;
                self.renaming = None;
                self.settings_scroll = 0.0;
            }
            Action::CloseSettings => {
                self.commit_rename(doc);
                self.font_picker = false;
                self.editing_scheme = None;
                self.view = if self.library.is_empty() { View::Library } else { View::Reader };
            }
            Action::SetSettingsTab(tab) => {
                self.commit_rename(doc);
                self.settings_tab = tab;
                self.settings_scroll = 0.0;
            }

            // --- settings screen: colour schemes -------------------------
            Action::SetScheme(id) => self.bump_setting(doc, |s| s.scheme_id = id),
            Action::ToggleSchemeEnabled(id) => {
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
            Action::DuplicateScheme(id) => {
                let mut sc = self.schemes.clone();
                if let Some(src) = sc.iter().find(|s| s.id == id).cloned() {
                    let ids: Vec<String> = sc.iter().map(|s| s.id.clone()).collect();
                    let new_id = Self::fresh_id("scheme", &ids);
                    let mut dup = src.clone();
                    dup.id = new_id.clone();
                    dup.name = format!("{} copy", src.name);
                    dup.builtin = false;
                    dup.enabled = true;
                    let pos = sc.iter().position(|s| s.id == id).map(|p| p + 1).unwrap_or(sc.len());
                    sc.insert(pos, dup);
                    BooksDoc::write_schemes(doc, &sc);
                    self.editing_scheme = Some(new_id);
                }
            }
            Action::DeleteScheme(id) => {
                let mut sc = self.schemes.clone();
                sc.retain(|s| s.builtin || s.id != id);
                BooksDoc::write_schemes(doc, &sc);
                if self.editing_scheme.as_deref() == Some(id.as_str()) {
                    self.editing_scheme = None;
                }
                if self.settings.scheme_id == id {
                    self.bump_setting(doc, |st| st.scheme_id = String::new());
                }
            }
            Action::EditScheme(id) => {
                self.commit_rename(doc);
                self.editing_scheme = Some(id);
            }
            Action::CloseSchemeEditor => {
                self.commit_rename(doc);
                self.editing_scheme = None;
            }
            Action::CycleSchemeColor(id, slot) => {
                let mut sc = self.schemes.clone();
                if let Some(s) = sc.iter_mut().find(|s| s.id == id && !s.builtin) {
                    let cur = s.slots()[slot].1;
                    s.set_slot(slot, next_ramp_color(cur));
                }
                BooksDoc::write_schemes(doc, &sc);
            }
            Action::NewSchemeFromBg(bg) => {
                let mut sc = self.schemes.clone();
                let ids: Vec<String> = sc.iter().map(|s| s.id.clone()).collect();
                let new_id = Self::fresh_id("scheme", &ids);
                let n = sc.iter().filter(|s| !s.builtin).count() + 1;
                let scheme = ColorScheme::from_bg(&new_id, &format!("Scheme {n}"), bg);
                sc.push(scheme);
                BooksDoc::write_schemes(doc, &sc);
                self.editing_scheme = Some(new_id.clone());
                self.bump_setting(doc, |st| st.scheme_id = new_id);
            }
            Action::BeginRenameScheme(id) => {
                self.commit_rename(doc);
                let name = self
                    .schemes
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                self.renaming = Some(Rename { target: RenameTarget::Scheme(id), buf: name });
            }

            // --- settings screen: fonts ----------------------------------
            Action::SetDefaultFont(id) => BooksDoc::write_default_font(doc, &id),
            Action::ToggleFontShown(id) => {
                let mut f = self.fonts.clone();
                if let Some(e) = f.iter_mut().find(|e| e.id == id) {
                    e.shown = !e.shown;
                }
                BooksDoc::write_fonts(doc, &f);
            }
            Action::FontMultInc(id) => {
                let mut f = self.fonts.clone();
                if let Some(e) = f.iter_mut().find(|e| e.id == id) {
                    e.size_mult = (e.size_mult + 0.05).clamp(FontEntry::MULT_MIN, FontEntry::MULT_MAX);
                }
                BooksDoc::write_fonts(doc, &f);
            }
            Action::FontMultDec(id) => {
                let mut f = self.fonts.clone();
                if let Some(e) = f.iter_mut().find(|e| e.id == id) {
                    e.size_mult = (e.size_mult - 0.05).clamp(FontEntry::MULT_MIN, FontEntry::MULT_MAX);
                }
                BooksDoc::write_fonts(doc, &f);
            }
            Action::RemoveFont(id) => {
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
            Action::OpenFontPicker => {
                self.font_picker = true;
                self.picker_scroll = 0.0;
            }
            Action::CloseFontPicker => self.font_picker = false,
            Action::AddSystemFont(family) => {
                let mut f = self.fonts.clone();
                let dup = f
                    .iter()
                    .any(|e| matches!(&e.kind, FontKind::Named(n) if n == &family));
                if !dup {
                    let ids: Vec<String> = f.iter().map(|e| e.id.clone()).collect();
                    let id = Self::fresh_id("font", &ids);
                    f.push(FontEntry::new(&id, FontKind::Named(family.clone()), &family, 1.0, true));
                    BooksDoc::write_fonts(doc, &f);
                }
                self.font_picker = false;
            }
            Action::BeginRenameFont(id) => {
                self.commit_rename(doc);
                let name = self
                    .fonts
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                self.renaming = Some(Rename { target: RenameTarget::Font(id), buf: name });
            }
        }
    }

    /// Commit an in-progress inline rename to the CRDT (a blank name is ignored,
    /// keeping the old one). Clears the rename regardless.
    fn commit_rename(&mut self, doc: &mut AutoCommit) {
        let Some(r) = self.renaming.take() else {
            return;
        };
        let name = r.buf.trim();
        if name.is_empty() {
            return;
        }
        match r.target {
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
        let choice = font_choice(&f.kind);
        let mono = f.kind.is_mono();
        (choice, f.size_mult, font_sig(&f), mono)
    }

    /// The colour-scheme pills shown in the quick-settings PAGE row: every
    /// **enabled** scheme (built-ins + custom). Disabling a scheme in the
    /// manager removes it here. Built-in tints route through `SetReadTheme` to
    /// keep their hand-tuned palette; custom schemes apply their own colours.
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
        if !self.settings.scheme_id.is_empty() {
            if let Some(s) = self
                .schemes
                .iter()
                .find(|s| s.id == self.settings.scheme_id && s.enabled && !s.builtin)
            {
                return ReaderPalette::from_scheme(s);
            }
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

    fn add_highlight(&mut self, doc: &mut AutoCommit, block: usize, color: HiColor) {
        let id = self.book_id();
        let excerpt = self.block_excerpt(block);
        let a = Annotation::new(&id, AnnoKind::Highlight, self.chapter, block)
            .with_color(color)
            .with_excerpt(excerpt);
        BooksDoc::upsert_annotation(doc, &a);
    }

    /// Highlight an arbitrary text selection: one ranged highlight per block the
    /// selection touches, each covering exactly the selected characters. This is
    /// what makes a drag across paragraphs mark precisely what was selected.
    fn add_highlight_range(&mut self, doc: &mut AutoCommit, sel: TextSel, color: HiColor) {
        let (start, end) = sel.ordered();
        let id = self.book_id();
        let Some(layout) = self.layout.as_ref() else { return };
        for b in &layout.blocks {
            if b.index < start.block || b.index > end.block {
                continue;
            }
            let lo = if b.index == start.block { start.ch } else { 0 };
            let hi = if b.index == end.block { end.ch } else { b.char_len };
            let lo = lo.min(b.char_len);
            let hi = hi.min(b.char_len);
            if lo >= hi {
                continue;
            }
            let excerpt: String = b.text.chars().skip(lo).take((hi - lo).min(120)).collect();
            let mut a = Annotation::new(&id, AnnoKind::Highlight, self.chapter, b.index)
                .with_color(color)
                .with_excerpt(excerpt);
            a.start = lo;
            a.end = hi;
            BooksDoc::upsert_annotation(doc, &a);
        }
    }

    /// The plain text covered by the current selection, across blocks (truncated
    /// for the annotation excerpt).
    fn selection_excerpt(&self) -> String {
        let Some(sel) = self.sel else { return String::new() };
        let (start, end) = sel.ordered();
        let Some(layout) = self.layout.as_ref() else { return String::new() };
        let mut out = String::new();
        for b in &layout.blocks {
            if b.index < start.block || b.index > end.block {
                continue;
            }
            let lo = if b.index == start.block { start.ch } else { 0 };
            let hi = if b.index == end.block { end.ch } else { b.char_len };
            let lo = lo.min(b.char_len);
            let hi = hi.min(b.char_len);
            if lo < hi {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.extend(b.text.chars().skip(lo).take(hi - lo));
            }
            if out.chars().count() > 160 {
                break;
            }
        }
        out.chars().take(160).collect()
    }

    fn clear_selection(&mut self) {
        self.sel = None;
        self.selected_block = None;
        self.dragging_sel = false;
    }

    /// Map a screen point to a [`DocPos`] using the run geometry recorded during
    /// the last draw. Returns `None` when there is no laid-out text (e.g. the
    /// shelf). Snaps to the nearest line, then the nearest character within it.
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
        let target = self
            .sel_runs
            .iter()
            .min_by(|a, b| vdist(a).partial_cmp(&vdist(b)).unwrap_or(std::cmp::Ordering::Equal))?;
        let ty0 = target.y0;
        let mut line: Vec<&SelRun> = self
            .sel_runs
            .iter()
            .filter(|r| (r.y0 - ty0).abs() < 0.5)
            .collect();
        line.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
        let first = line.first()?;
        let last = line.last()?;
        if p.x <= first.x0 {
            return Some(DocPos { block: first.block, ch: first.cs });
        }
        if p.x >= last.x1 {
            return Some(DocPos { block: last.block, ch: last.cs + last.cl });
        }
        for r in &line {
            if p.x >= r.x0 && p.x < r.x1 {
                let w = (r.x1 - r.x0).max(1.0);
                let frac = ((p.x - r.x0) / w).clamp(0.0, 1.0);
                let k = (frac * r.cl as f64).round() as usize;
                return Some(DocPos { block: r.block, ch: r.cs + k.min(r.cl) });
            }
        }
        // In a gap between words: snap to the nearest run edge.
        let nearest = line.iter().min_by(|a, b| {
            let da = (p.x - (a.x0 + a.x1) / 2.0).abs();
            let db = (p.x - (b.x0 + b.x1) / 2.0).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let ch = if p.x < nearest.x0 { nearest.cs } else { nearest.cs + nearest.cl };
        Some(DocPos { block: nearest.block, ch })
    }

    fn toggle_bookmark(&mut self, doc: &mut AutoCommit) {
        let block = self.page_top_block;
        let existing = self.annotations.iter().find(|a| {
            a.kind == AnnoKind::Bookmark && a.chapter == self.chapter && a.block == block
        });
        if let Some(a) = existing {
            let id = a.id;
            BooksDoc::remove_annotation(doc, id);
        } else {
            let id = self.book_id();
            let excerpt = self.block_excerpt(block);
            let a = Annotation::new(&id, AnnoKind::Bookmark, self.chapter, block)
                .with_excerpt(excerpt);
            BooksDoc::upsert_annotation(doc, &a);
        }
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

    // ======================================================================
    // Hit-test helpers
    // ======================================================================

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

/// One PAGE pill in the quick-settings popover: a colour scheme the reader can
/// switch to. Built-in tints and (later) enabled custom schemes both flow
/// through this so the popover doesn't care which kind it's drawing.
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

// ==========================================================================
// Widget impl
// ==========================================================================

impl Widget for BooksApp {
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(s) = self.shot_settings.take() {
            BooksDoc::write_settings(doc, &s);
        }
        if let Some(f) = self.shot_fonts.take() {
            BooksDoc::write_fonts(doc, &f);
        }
        if let Some(sc) = self.shot_schemes.take() {
            BooksDoc::write_schemes(doc, &sc);
        }
        self.seed_if_needed(doc);
        if self.shot_compose && self.composing.is_none() {
            self.shot_compose = false;
            if let Some(sel) = self.sel {
                let (start, _) = sel.ordered();
                let bid = self.book_id();
                let mut a = Annotation::new(&bid, AnnoKind::Note, self.chapter, start.block)
                    .with_color(HiColor::Blue)
                    .with_excerpt("the keeper climbed the iron stair and set the great wick alight");
                a.note = "A note, written right here on the page — no separate screen.".into();
                let aid = a.id;
                BooksDoc::upsert_annotation(doc, &a);
                self.composing = Some(aid);
            }
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
        node_map.insert(node, self as *mut dyn Widget);
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
        // Advance the rail flap transitions (reconcile first so a resize this
        // frame uses the right mode default) before laying anything out.
        self.reconcile_flaps();
        let dt = 1.0 / 60.0;
        self.toc_flap.tick(self.toc_layout(), dt);
        self.notes_flap.tick(self.notes_layout(), dt);
        // App-base fill so nothing shows through.
        paint::fill_rect(scene, transform, self.rect, theme.tokens.colors.surfaces.base);
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
        _modifiers: &Modifiers,
        doc: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                if p != self.cursor {
                    self.cursor = p;
                    self.dirty = true; // hover feedback
                    // Drag text selection: once the press has moved far enough,
                    // the head follows the pointer and the selection spans
                    // whatever paragraphs it covers. Dragging into the top/bottom
                    // margin auto-advances a page so a selection can span pages.
                    if self.mouse_down && self.press_doc.is_some() && self.view == View::Reader {
                        if !self.dragging_sel && (p - self.press_pos).hypot() > 4.0 {
                            self.dragging_sel = true;
                        }
                        if self.dragging_sel {
                            if let Some(head) = self.hit_doc_pos(p) {
                                let anchor = self.press_doc.unwrap();
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
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Topmost wins: later-recorded hits draw on top.
                let top = self
                    .hits
                    .iter()
                    .rev()
                    .find(|h| h.rect.contains(self.cursor))
                    .cloned();
                // A press on page text starts a potential drag selection.
                self.mouse_down = true;
                self.dragging_sel = false;
                self.press_pos = self.cursor;
                let on_page = self.view == View::Reader
                    && top.as_ref().map_or(true, |h| matches!(h.action, Action::SelectBlock(_)));
                self.press_doc = if on_page { self.hit_doc_pos(self.cursor) } else { None };
                if on_page {
                    self.sel = None; // fresh press clears any prior selection
                    if top.is_none() {
                        self.selected_block = None;
                    }
                }
                if let Some(h) = top {
                    self.dispatch(h.action, doc);
                    self.refresh_from_doc(doc);
                    self.persist_progress(doc);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.mouse_down = false;
                if self.dragging_sel {
                    self.dragging_sel = false;
                    if let Some(s) = self.sel {
                        if s.is_empty() {
                            self.sel = None;
                            self.selected_block = None;
                        }
                    }
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if ke.state == ElementState::Pressed =>
            {
                if self.handle_key(&ke.logical_key, doc) {
                    self.refresh_from_doc(doc);
                    self.persist_progress(doc);
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // A line notch is one page/row; a trackpad's pixel deltas are
                // summed so a flick is a controlled turn, not a stampede. A
                // dominantly *horizontal* gesture (two-finger trackpad swipe or
                // a touch swipe surfaced as scroll) turns pages left/right.
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (*x as f64 * WHEEL_LINE_PX, (-*y as f64) * WHEEL_LINE_PX)
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
            _ => {}
        }
    }

    fn name(&self) -> &str {
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
        // Keep the event loop pumping frames while either rail flap is
        // mid-slide so the overlay reveal + scrim fade are smooth.
        self.flaps_animating()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl BooksApp {
    /// Library-shelf keyboard navigation. A directional cursor moves across the
    /// grid (up/down step by a whole row, using the column count the last draw
    /// resolved); Enter/Space opens the focused book. Returns `true` when the
    /// key was consumed. Without this the shelf was pointer-only.
    fn handle_library_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
        let n = self.library.len();
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
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.dispatch(Action::OpenBook(cursor), doc);
                true
            }
            _ => false,
        }
    }

    /// Keyboard handling. Returns `true` if the key was consumed (so the App's
    /// focus-navigation fallback doesn't also steal the arrows).
    fn handle_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
        // An inline rename (settings screen) captures typing first.
        if self.renaming.is_some() {
            match key {
                Key::Named(NamedKey::Enter) => {
                    self.commit_rename(doc);
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.renaming = None;
                    return true;
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(r) = &mut self.renaming {
                        r.buf.pop();
                    }
                    return true;
                }
                Key::Named(NamedKey::Space) => {
                    if let Some(r) = &mut self.renaming {
                        r.buf.push(' ');
                    }
                    return true;
                }
                Key::Character(c) => {
                    if let Some(r) = &mut self.renaming {
                        r.buf.push_str(c);
                    }
                    return true;
                }
                _ => return false,
            }
        }

        // The settings screen has its own minimal key handling (Escape peels
        // back overlays, then leaves); no reader/library shortcuts apply there.
        if self.view == View::Settings {
            if let Key::Named(NamedKey::Escape) = key {
                if self.font_picker {
                    self.font_picker = false;
                } else if self.editing_scheme.is_some() {
                    self.editing_scheme = None;
                } else {
                    self.dispatch(Action::CloseSettings, doc);
                }
                return true;
            }
            return false;
        }

        // While composing a note, characters feed the note text.
        if self.composing.is_some() {
            match key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                    return self.edit_note_text(doc, NoteEdit::Done);
                }
                Key::Named(NamedKey::Backspace) => {
                    return self.edit_note_text(doc, NoteEdit::Backspace);
                }
                Key::Named(NamedKey::Space) => {
                    return self.edit_note_text(doc, NoteEdit::Insert(" ".into()));
                }
                Key::Character(c) => {
                    return self.edit_note_text(doc, NoteEdit::Insert(c.to_string()));
                }
                _ => return false,
            }
        }

        if self.view == View::Library {
            return self.handle_library_key(key, doc);
        }

        match key {
            Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::PageUp) => {
                self.dispatch(Action::PrevPage, doc);
                true
            }
            Key::Named(NamedKey::ArrowRight)
            | Key::Named(NamedKey::PageDown)
            | Key::Named(NamedKey::Space) => {
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
                if self.type_popover {
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
                    "h" | "H" => self.selected_block.map(|_| Action::SetColor(self.hi_color)),
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

    /// Horizontal swipe handling. A swipe left (positive travel) turns to the
    /// next page; a swipe right turns back — the natural page-flip mapping.
    /// Accumulated so a trackpad's many small deltas coalesce into whole turns.
    /// Suppressed on the shelf and while a modal surface owns input. Returns
    /// `true` when a page actually turned.
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

// ==========================================================================
// Small chrome-drawing primitives
// ==========================================================================

impl BooksApp {
    /// A square icon button. Returns the x-advance (its size).
    #[allow(clippy::too_many_arguments)]
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
            paint::fill_round(scene, t, rect, theme.tokens.radii.md as f64, theme.accent_primary);
        } else if hover {
            paint::fill_round(scene, t, rect, theme.tokens.radii.md as f64, theme.bg_hover);
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
            Ico::Back | Ico::Prev => paint::chevron_left(scene, t, inner, icon_col, w),
            Ico::Next => paint::chevron_right(scene, t, inner, icon_col, w),
            Ico::Toc => paint::list(scene, t, inner, icon_col, w),
            Ico::Notes => paint::note(scene, t, inner, icon_col, w),
            Ico::Bookmark(filled) => paint::bookmark(scene, t, inner, icon_col, w, filled),
            Ico::PlusI => paint::plus(scene, t, inner, icon_col, w),
            Ico::MinusI => paint::minus(scene, t, inner, icon_col, w),
        }
        self.push_hit(rect, action);
        s
    }

    /// A text pill (used for segmented controls + tags). Returns its width.
    #[allow(clippy::too_many_arguments)]
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
        let w = tw as f64 + pad * 2.0;
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
        let ty = y + (h - size as f64) / 2.0;
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

// ==========================================================================
// Library (shelf) view
// ==========================================================================

impl BooksApp {
    fn draw_library(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let lib = std::mem::take(&mut self.library);
        let c = theme.tokens.colors.clone();
        let r = self.rect;

        // --- top bar ------------------------------------------------------
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
            21.0,
            c.text_primary,
            t * Affine::translate((r.x0 + 28.0, r.y0 + 15.0)),
            None,
            TextWeight::Bold,
        );
        let sub = match lib.len() {
            1 => "1 book".to_string(),
            n => format!("{n} books"),
        };
        let (sw, _) = tr.measure(&sub, 13.0, None);
        tr.draw_text(
            scene,
            &sub,
            13.0,
            c.text_dim,
            t * Affine::translate((r.x1 - 28.0 - sw as f64, r.y0 + 19.0)),
            None,
        );

        // --- empty shelf --------------------------------------------------
        if lib.is_empty() {
            let cx = (r.x0 + r.x1) / 2.0;
            let cy = (r.y0 + TOP_BAR + r.y1) / 2.0;
            let head = "No books yet";
            let (hw, _) = tr.measure_weighted(head, 18.0, None, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                head,
                18.0,
                c.text_primary,
                t * Affine::translate((cx - hw as f64 / 2.0, cy - 24.0)),
                None,
                TextWeight::Semibold,
            );
            let hint = "Add .epub files to your Books folder, then reload.";
            let (tw2, _) = tr.measure(hint, 13.0, None);
            tr.draw_text(
                scene,
                hint,
                13.0,
                c.text_dim,
                t * Affine::translate((cx - tw2 as f64 / 2.0, cy + 4.0)),
                None,
            );
            // A real, in-app action: re-scan the library directories so books
            // dropped in after launch appear without restarting. (Browsing for a
            // file needs a shell file-picker portal, which lives outside the app.)
            let label = "Reload library";
            let (bw, _) = tr.measure_weighted(label, 13.0, None, TextWeight::Medium);
            let bh = 34.0;
            let bwidth = bw as f64 + 34.0;
            let bx = cx - bwidth / 2.0;
            let by = cy + 30.0;
            let brect = Rect::new(bx, by, bx + bwidth, by + bh);
            let bhover = self.is_hover(brect);
            paint::fill_round(
                scene,
                t,
                brect,
                bh / 2.0,
                if bhover { theme.bg_hover } else { c.surfaces.overlay },
            );
            paint::stroke_round(scene, t, brect, bh / 2.0, c.border_standard, 1.0);
            tr.draw_text_weighted(
                scene,
                label,
                13.0,
                c.text_primary,
                t * Affine::translate((bx + 17.0, by + (bh - 13.0) / 2.0)),
                None,
                TextWeight::Medium,
            );
            self.push_hit(brect, Action::ReloadLibrary);
            self.library = lib;
            return;
        }

        // --- grid ---------------------------------------------------------
        let margin = 40.0;
        let gap = 28.0;
        let card_w = 196.0;
        let cover_h = 232.0;
        let card_h = cover_h + 78.0;
        let avail = (r.width() - 2.0 * margin + gap).max(card_w);
        let cols = (avail / (card_w + gap)).floor().max(1.0) as usize;
        // Publish the column count so keyboard up/down can step a whole row.
        self.shelf_cols = cols;
        self.shelf_cursor = self.shelf_cursor.min(lib.len().saturating_sub(1));
        let grid_w = cols as f64 * card_w + (cols as f64 - 1.0) * gap;
        let start_x = r.x0 + (r.width() - grid_w) / 2.0;
        let start_y = r.y0 + TOP_BAR + 36.0;

        // The shelf can hold more books than fit, so it scrolls. Work out the
        // total content height and clamp the offset to what's reachable.
        let rows = lib.len().div_ceil(cols);
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
        let scroll = self.shelf_scroll;

        // Clip the scrolling body to below the top bar so cards slide under it
        // rather than over it, then offset every card by the scroll.
        let body = Rect::new(r.x0, r.y0 + TOP_BAR + 1.0, r.x1, r.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        for (i, book) in lib.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x = start_x + col as f64 * (card_w + gap);
            let y = start_y + row as f64 * row_pitch - scroll;
            // Cull rows fully outside the viewport (keeps a huge shelf cheap).
            if y > r.y1 || y + card_h + 84.0 < r.y0 + TOP_BAR {
                continue;
            }
            let focused = i == self.shelf_cursor;
            self.draw_book_card(scene, t, tr, theme, book, i, x, y, card_w, cover_h, focused);
        }
        scene.pop_layer();

        // Scroll indicator, only when there's something to scroll to.
        if self.shelf_max_scroll > 1.0 {
            let track_x = r.x1 - 9.0;
            let track_y0 = r.y0 + TOP_BAR + 10.0;
            let track_y1 = r.y1 - 10.0;
            let track_h = (track_y1 - track_y0).max(1.0);
            let thumb_h = (track_h * visible_h / content_h).clamp(28.0, track_h);
            let frac = scroll / self.shelf_max_scroll;
            let thumb_y = track_y0 + frac * (track_h - thumb_h);
            let thumb = Rect::new(track_x, thumb_y, track_x + 4.0, thumb_y + thumb_h);
            paint::fill_round(scene, t, thumb, 2.0, c.text_dim.with_alpha_factor(0.5));
        }

        self.library = lib;
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

    #[allow(clippy::too_many_arguments)]
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
    ) {
        let c = &theme.tokens.colors;
        let cover = Rect::new(x, y, x + w, y + cover_h);
        let hover = self.is_hover(Rect::new(x, y, x + w, y + cover_h + 78.0));

        // Drop shadow.
        let shadow = Rect::new(x + 3.0, y + 6.0, x + w + 3.0, y + cover_h + 6.0);
        paint::fill_round(scene, t, shadow, 12.0, Color::from_rgba8(0, 0, 0, 60));

        // Real cover art when the EPUB carries one; otherwise a generated spine
        // built from theme hues + the title/author set in type.
        if let Some(img) = self.cover_image(book) {
            blit_cover(scene, t, img, cover, 12.0);
            // Faint inner edge to seat the art against the shelf.
            paint::stroke_round(scene, t, cover, 12.0, Color::from_rgba8(0, 0, 0, 40), 1.0);
        } else {
            let (spine, ink) = cover_colors(theme, book.cover_tint);
            paint::fill_round(scene, t, cover, 12.0, spine);
            // a darker foot for depth
            let foot = Rect::new(x, y + cover_h * 0.72, x + w, y + cover_h);
            paint::fill_round(scene, t, foot, 12.0, Color::from_rgba8(0, 0, 0, 38));

            // subject pill + spark.
            if !book.subject.is_empty() {
                let psize = 11.0;
                let (pw, _) = tr.measure(&book.subject, psize, None);
                let pill = Rect::new(x + 16.0, y + 16.0, x + 16.0 + pw as f64 + 18.0, y + 16.0 + 20.0);
                paint::fill_round(scene, t, pill, 10.0, ink.with_alpha_factor(0.16));
                tr.draw_text(
                    scene,
                    &book.subject,
                    psize,
                    ink,
                    t * Affine::translate((x + 25.0, y + 20.0)),
                    None,
                );
            }
            paint::spark(scene, t, Point::new(x + w - 26.0, y + 26.0), 7.0, ink.with_alpha_factor(0.8), 1.6);

            // title (wrapped) + author.
            let title_w = w - 32.0;
            tr.draw_text_weighted(
                scene,
                &book.title,
                19.0,
                ink,
                t * Affine::translate((x + 16.0, y + cover_h * 0.40)),
                Some(title_w as f32),
                TextWeight::Bold,
            );
            tr.draw_text(
                scene,
                &book.author,
                13.0,
                ink.with_alpha_factor(0.82),
                t * Affine::translate((x + 16.0, y + cover_h - 30.0)),
                Some(title_w as f32),
            );
        }
        // Keyboard focus draws a ring even without a pointer over the card, so
        // arrow-key navigation is visible; hover uses the same treatment.
        if focused {
            paint::stroke_round(scene, t, cover, 12.0, theme.accent_primary, 2.5);
        } else if hover {
            paint::stroke_round(scene, t, cover, 12.0, theme.border_focus, 2.0);
        }

        // --- under-cover meta: progress + counts -------------------------
        let frac = self.shelf_fraction.get(&book.id).copied().unwrap_or(0.0);
        let anno = self.shelf_annos.get(&book.id).copied().unwrap_or(0);
        let meta_y = y + cover_h + 14.0;
        tr.draw_text_weighted(
            scene,
            &book.title,
            14.0,
            c.text_primary,
            t * Affine::translate((x + 2.0, meta_y)),
            Some((w - 4.0) as f32),
            TextWeight::Semibold,
        );
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
            let badge = Rect::new(x + 2.0, meta_y + 50.0, x + 2.0 + lw as f64 + 16.0, meta_y + 50.0 + 18.0);
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

        // Clamp the click target to the scrolling body so a card sliding under
        // the top bar can't capture clicks up in the header.
        let hit_top = y.max(self.rect.y0 + TOP_BAR);
        let hit_bot = (meta_y + 70.0).min(self.rect.y1);
        if hit_bot > hit_top {
            self.push_hit(Rect::new(x, hit_top, x + w, hit_bot), Action::OpenBook(idx));
        }
    }
}

// ==========================================================================
// Reader view
// ==========================================================================

impl BooksApp {
    fn draw_reader(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let lib = std::mem::take(&mut self.library);
        let annos = std::mem::take(&mut self.annotations);
        let c = theme.tokens.colors.clone();
        let r = self.rect;
        self.book_idx = self.book_idx.min(lib.len().saturating_sub(1));
        let nchap = lib.get(self.book_idx).map(|b| b.chapters.len()).unwrap_or(0);
        self.chapter = self.chapter.min(nchap.saturating_sub(1));

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
        let body_y1 = r.y1 - BOTTOM_BAR;

        let palette = self.resolve_palette(theme);

        // ---- paper rect + content box -----------------------------------
        // On a small screen the book *is* the focus: it fills the whole reading
        // area edge-to-edge, with no framed paper card or margin (that framing
        // only earns its keep on a wide window where the page would otherwise
        // sprawl). On a wide screen we keep the floating paper card.
        let framed = self.rect.width() >= FRAME_MIN_W;
        let center_w = (center_x1 - center_x0).max(120.0);
        let (paper_x, paper_y, paper_w, paper_h, pad_x, pad_top, pad_bot) = if framed {
            let paper_w = (center_w - 56.0).min(PAGE_MAX_W).max(120.0);
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
        let content_x = paper_x + pad_x;
        let content_y = paper_y + pad_top;
        let content_w = (paper_w - 2.0 * pad_x).max(60.0) as f32;
        let content_h = (paper_h - pad_top - pad_bot).max(60.0) as f32;

        // ---- (re)compute the chapter layout -----------------------------
        let (base_family, font_mult, font_sig, base_mono) = self.body_font();
        let key = LayoutKey::new(self.chapter, content_w, content_h, &self.settings, font_sig);
        if self.layout.as_ref().map(|l| l.key) != Some(key) {
            if let Some(book) = lib.get(self.book_idx) {
                if let Some(ch) = book.chapters.get(self.chapter) {
                    self.layout = Some(layout_chapter(
                        tr,
                        &ch.blocks,
                        content_w,
                        content_h,
                        &self.settings,
                        self.chapter,
                        &base_family,
                        font_mult,
                        font_sig,
                    ));
                }
            }
        }
        let layout = self.layout.take().unwrap_or_else(|| ChapterLayout {
            blocks: Vec::new(),
            total_height: 0.0,
            pages: vec![0.0],
            key,
        });
        self.cur_page_count = layout.page_count();
        if self.pending_last_page {
            self.page = layout.page_count().saturating_sub(1);
            self.pending_last_page = false;
        }
        if let Some(jb) = self.pending_jump_block.take() {
            self.page = page_for_block(&layout, jb);
        }
        self.page = self.page.min(layout.page_count().saturating_sub(1));

        let page_start = layout.pages.get(self.page).copied().unwrap_or(0.0);
        let page_end = layout
            .pages
            .get(self.page + 1)
            .copied()
            .unwrap_or(layout.total_height + 1.0);

        // first visible block → bookmark target
        self.page_top_block = layout
            .blocks
            .iter()
            .find(|b| b.lines.iter().any(|l| l.y >= page_start - 0.5 && l.y < page_end))
            .map(|b| b.index)
            .unwrap_or(0);

        // ---- page surface ------------------------------------------------
        let paper = Rect::new(paper_x, paper_y, paper_x + paper_w, paper_y + paper_h);
        if framed {
            let shadow = Rect::new(paper_x - 2.0, paper_y + 4.0, paper_x + paper_w + 2.0, paper_y + paper_h + 8.0);
            paint::fill_round(scene, t, shadow, 10.0, Color::from_rgba8(0, 0, 0, 64));
            paint::fill_round(scene, t, paper, 8.0, palette.page);
            paint::stroke_round(scene, t, paper, 8.0, palette.page_edge, 1.0);
        } else {
            // Borderless: the page fills the reading area, no card or shadow.
            paint::fill_rect(scene, t, paper, palette.page);
        }

        // running header (chapter title) + rule — a framed-page flourish only;
        // a borderless small screen drops it to hand every pixel to the text
        // (the chapter is still named in the bottom chrome bar).
        if framed {
            if let Some(book) = lib.get(self.book_idx) {
                if let Some(ch) = book.chapters.get(self.chapter) {
                    let head = ch.title.to_uppercase();
                    tr.draw_text_weighted(
                        scene,
                        &head,
                        11.0,
                        palette.ink_dim,
                        t * Affine::translate((content_x, paper_y + 22.0)),
                        Some(content_w),
                        TextWeight::Semibold,
                    );
                }
            }
            paint::line(
                scene,
                t,
                Point::new(content_x, paper_y + 42.0),
                Point::new(content_x + content_w as f64, paper_y + 42.0),
                palette.rule,
                1.0,
            );
        }

        // ---- highlight bands + margin glyphs (behind text) --------------
        let chapter = self.chapter;
        // Screen-y of the selected block's top, if it is on this page — used to
        // anchor the floating selection toolbar.
        let mut sel_anchor: Option<f64> = None;
        for b in &layout.blocks {
            let visible: Vec<&crate::paginate::LaidLine> = b
                .lines
                .iter()
                .filter(|l| l.y >= page_start - 0.5 && l.y < page_end)
                .collect();
            if visible.is_empty() {
                continue;
            }
            let by0 = content_y + (visible.first().unwrap().y - page_start) as f64 - 3.0;
            let by1 = content_y + (visible.last().unwrap().y - page_start) as f64 + b.line_height as f64;

            // selected-block outline (only for a plain block tap; a drag text
            // selection renders as bands instead, painted in the selection pass)
            let sel_active = self.sel.map_or(false, |s| !s.is_empty());
            if self.selected_block == Some(b.index) && !sel_active {
                let sel = Rect::new(
                    content_x + b.indent as f64 - 8.0,
                    by0 - 2.0,
                    content_x + content_w as f64 + 6.0,
                    by1 + 2.0,
                );
                paint::fill_round(scene, t, sel, 6.0, palette.accent.with_alpha_factor(0.10));
                paint::stroke_round(scene, t, sel, 6.0, palette.accent.with_alpha_factor(0.55), 1.2);
                sel_anchor = Some(by0);
            }

            for a in annos.iter().filter(|a| a.chapter == chapter && a.block == b.index) {
                match a.kind {
                    // A whole-block highlight (start==end) fills each line; a
                    // *ranged* highlight is painted precisely in the selection
                    // pass, so skip it here.
                    AnnoKind::Highlight if a.start >= a.end => {
                        for l in &visible {
                            let ly = content_y + (l.y - page_start) as f64;
                            let band = Rect::new(
                                content_x + b.indent as f64 - 4.0,
                                ly - 1.0,
                                content_x + content_w as f64,
                                ly + b.line_height as f64 - 2.0,
                            );
                            paint::fill_round(scene, t, band, 4.0, palette.highlight_fill(a.color));
                        }
                    }
                    AnnoKind::Highlight => {}
                    AnnoKind::Note => {
                        // faint band + accent left bar + margin glyph
                        let band = Rect::new(content_x + b.indent as f64 - 4.0, by0, content_x + content_w as f64, by1);
                        paint::fill_round(scene, t, band, 4.0, palette.highlight_fill(a.color).with_alpha_factor(0.5));
                        let bar = Rect::new(content_x + b.indent as f64 - 10.0, by0, content_x + b.indent as f64 - 6.0, by1);
                        paint::fill_rect(scene, t, bar, ReaderPalette::highlight_swatch(a.color));
                        let gx = paper_x + paper_w - 30.0;
                        paint::note(scene, t, Rect::new(gx, by0, gx + 20.0, by0 + 20.0), palette.ink_dim, 1.6);
                    }
                    AnnoKind::Bookmark => {
                        let gx = paper_x + paper_w - 30.0;
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

            // whole-block click target (select for annotation)
            if !matches!(b.kind, LayoutKind::Separator) {
                self.push_hit(
                    Rect::new(content_x - 8.0, by0, content_x + content_w as f64 + 8.0, by1),
                    Action::SelectBlock(b.index),
                );
            }
        }

        // ---- selection pass: run geometry + selection / ranged-highlight
        // bands, painted behind the glyphs. Rebuilds `sel_runs` for pointer
        // hit-testing and records the page's first/last doc position. ------
        self.reader_body = (body_y0, body_y1);
        self.sel_runs.clear();
        self.page_first_doc = None;
        self.page_last_doc = None;
        let sel_ord = self.sel.filter(|s| !s.is_empty()).map(|s| s.ordered());
        let ranged: Vec<&Annotation> = annos
            .iter()
            .filter(|a| a.chapter == chapter && a.kind == AnnoKind::Highlight && a.start < a.end)
            .collect();
        for b in &layout.blocks {
            if matches!(b.kind, LayoutKind::Separator) {
                continue;
            }
            let indent = b.indent as f64;
            for l in &b.lines {
                if l.y < page_start - 0.5 || l.y >= page_end {
                    continue;
                }
                let ly = content_y + (l.y - page_start) as f64;
                let lh = b.line_height as f64;
                // Per-line selection band extent.
                let mut sel_band: Option<(f64, f64)> = None;
                for run in &l.runs {
                    let weight = if run.bold { TextWeight::Bold } else { TextWeight::Regular };
                    let fam = if run.mono || base_mono { FontChoice::Monospace } else { base_family.clone() };
                    let (w, _) = tr.measure_font(&run.text, b.size, None, weight, run.italic, &fam);
                    let x0 = content_x + indent + run.x as f64;
                    let x1 = x0 + w as f64;
                    self.sel_runs.push(SelRun { block: b.index, x0, x1, y0: ly, y1: ly + lh, cs: run.char_start, cl: run.char_len });
                    if self.page_first_doc.is_none() {
                        self.page_first_doc = Some(DocPos { block: b.index, ch: run.char_start });
                    }
                    self.page_last_doc = Some(DocPos { block: b.index, ch: run.char_start + run.char_len });
                    let run_end = run.char_start + run.char_len;
                    // Selection sub-range within this run.
                    if let Some((s, e)) = sel_ord {
                        if b.index >= s.block && b.index <= e.block {
                            let lo = if b.index == s.block { s.ch.max(run.char_start) } else { run.char_start };
                            let hi = if b.index == e.block { e.ch.min(run_end) } else { run_end };
                            if lo < hi {
                                let (bx0, bx1) = run_sub_x(x0, w as f64, run.char_start, run.char_len, lo, hi);
                                sel_band = Some(match sel_band {
                                    Some((a, b2)) => (a.min(bx0), b2.max(bx1)),
                                    None => (bx0, bx1),
                                });
                                if sel_anchor.is_none() {
                                    sel_anchor = Some(ly);
                                }
                            }
                        }
                    }
                    // Ranged highlight bands (precise, per run).
                    for a in ranged.iter().filter(|a| a.block == b.index) {
                        let lo = a.start.max(run.char_start);
                        let hi = a.end.min(run_end);
                        if lo < hi {
                            let (bx0, bx1) = run_sub_x(x0, w as f64, run.char_start, run.char_len, lo, hi);
                            let band = Rect::new(bx0 - 1.0, ly - 1.0, bx1 + 1.0, ly + lh - 2.0);
                            paint::fill_round(scene, t, band, 4.0, palette.highlight_fill(a.color));
                        }
                    }
                }
                if let Some((mnx, mxx)) = sel_band {
                    let band = Rect::new(mnx - 2.0, ly - 1.0, mxx + 2.0, ly + lh - 2.0);
                    paint::fill_round(scene, t, band, 3.0, palette.accent.with_alpha_factor(0.28));
                }
            }
        }
        // A drag that auto-advanced a page extends the head to this page's edge.
        if let Some(to_end) = self.pending_sel_edge.take() {
            if let Some(mut sel) = self.sel {
                let edge = if to_end { self.page_last_doc } else { self.page_first_doc };
                if let Some(edge) = edge {
                    sel.head = edge;
                    self.sel = Some(sel);
                    let (start, _) = sel.ordered();
                    self.selected_block = Some(start.block);
                }
            }
        }

        // ---- text + decorations (on top of bands) -----------------------
        for b in &layout.blocks {
            // quote bar / separator rule
            match b.kind {
                LayoutKind::Quote => {
                    let visible: Vec<&crate::paginate::LaidLine> = b
                        .lines
                        .iter()
                        .filter(|l| l.y >= page_start - 0.5 && l.y < page_end)
                        .collect();
                    if let (Some(f), Some(la)) = (visible.first(), visible.last()) {
                        let y0 = content_y + (f.y - page_start) as f64;
                        let y1 = content_y + (la.y - page_start) as f64 + b.line_height as f64;
                        let bar = Rect::new(content_x + 10.0, y0, content_x + 13.5, y1);
                        paint::fill_rect(scene, t, bar, palette.rule);
                    }
                }
                LayoutKind::Separator => {
                    if b.top >= page_start - 0.5 && b.top < page_end {
                        let cy = content_y + (b.top - page_start) as f64 + b.line_height as f64 / 2.0;
                        let cx = content_x + content_w as f64 / 2.0;
                        paint::line(
                            scene,
                            t,
                            Point::new(cx - 28.0, cy),
                            Point::new(cx + 28.0, cy),
                            palette.rule,
                            1.4,
                        );
                    }
                }
                _ => {}
            }

            // bullet
            if let Some(bullet) = &b.bullet {
                if let Some(first) = b.lines.iter().find(|l| l.y >= page_start - 0.5 && l.y < page_end) {
                    let ly = content_y + (first.y - page_start) as f64;
                    tr.draw_text(
                        scene,
                        bullet,
                        b.size,
                        palette.ink_dim,
                        t * Affine::translate((content_x + b.indent as f64 - 20.0, ly)),
                        None,
                    );
                }
            }

            for l in &b.lines {
                if l.y < page_start - 0.5 || l.y >= page_end {
                    continue;
                }
                let ly = content_y + (l.y - page_start) as f64;
                for run in &l.runs {
                    let mono = run.mono || base_mono;
                    let weight = if run.bold { TextWeight::Bold } else { TextWeight::Regular };
                    let x = content_x + b.indent as f64 + run.x as f64;
                    let fam = if mono { FontChoice::Monospace } else { base_family.clone() };
                    // inline-code chip
                    if run.mono && !base_mono {
                        let (cw, _) = tr.measure_styled(&run.text, b.size, None, weight, run.italic, true);
                        let chip = Rect::new(x - 3.0, ly - 1.0, x + cw as f64 + 3.0, ly + b.size as f64 + 3.0);
                        paint::fill_round(scene, t, chip, 3.0, palette.code_bg);
                    }
                    tr.draw_text_font(
                        scene,
                        &run.text,
                        b.size,
                        palette.ink,
                        t * Affine::translate((x, ly)),
                        None,
                        weight,
                        run.italic,
                        &fam,
                    );
                }
            }
        }

        // ---- page footer -------------------------------------------------
        // The page stays clean: only a thin book-progress bar. The page/chapter
        // locators live in the bottom chrome bar, so an on-page "Page X · Chapter
        // Y" footer would just duplicate them next to this bar.
        let frac = if nchap > 0 {
            (self.chapter as f32 + (self.page as f32 + 1.0) / layout.page_count() as f32) / nchap as f32
        } else {
            0.0
        };
        let pbar = Rect::new(content_x, paper_y + paper_h - 16.0, content_x + content_w as f64, paper_y + paper_h - 13.0);
        paint::progress(scene, t, pbar, frac, palette.rule, palette.accent);

        // restore the layout cache (key drives reuse next frame)
        self.layout = Some(layout);
        // restore the annotation cache before chrome reads it (the top bar's
        // bookmark state + the notes drawer both consult `self.annotations`)
        self.annotations = annos;

        // ---- chrome: top bar, bottom bar, selection HUD, drawers --------
        self.draw_reader_topbar(scene, t, tr, theme, &lib, &c);
        self.draw_reader_bottombar(scene, t, tr, theme, &c, nchap.max(1));
        // Floating selection toolbar over the page (under any overlay rail).
        self.draw_selection_hud(scene, t, tr, theme, &c, paper_x, paper_w, body_y0, sel_anchor);
        self.draw_rails(scene, t, tr, theme, &lib, body_y0, body_y1, &c);
        // The on-page note composer (anchored near the passage) floats over the
        // page instead of shunting note-writing off to a separate screen.
        self.draw_note_composer(scene, t, tr, theme, &c, sel_anchor);
        // Reading-settings popover floats above everything.
        self.draw_type_popover(scene, t, tr, theme, &c);

        self.library = lib;
    }

    /// A floating, contextual toolbar shown while a paragraph is selected:
    /// five highlight colours (a tap highlights the selection in that colour)
    /// and a Note action. Reachable at any width — this is how highlighting
    /// works when the annotations rail is collapsed on a narrow window.
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
        anchor_y: Option<f64>,
    ) {
        // Hide the picker while a note is being composed (the composer takes
        // over) or when there's no active selection/block.
        if self.view != View::Reader || self.selected_block.is_none() || self.composing.is_some() {
            return;
        }
        let r = self.rect;

        let pad = 12.0;
        let dot_r = 9.0;
        let dot_gap = 10.0;
        let hud_h = 42.0;
        let dots_w = HiColor::ALL.len() as f64 * (dot_r * 2.0) + (HiColor::ALL.len() as f64 - 1.0) * dot_gap;
        // "Note" action pill.
        let (nw, _) = tr.measure_weighted("Note", 13.0, None, TextWeight::Medium);
        let note_w = nw as f64 + 24.0;
        let sep_w = 13.0;
        let hud_w = pad * 2.0 + dots_w + sep_w + note_w;

        let cx = paper_x + paper_w / 2.0;
        let hud_x = (cx - hud_w / 2.0).clamp(r.x0 + 8.0, r.x1 - 8.0 - hud_w);
        // Prefer floating just above the selected block; drop below if that
        // would collide with the top bar; fall back to the page top.
        let hud_y = match anchor_y {
            Some(ay) if ay - hud_h - 12.0 >= body_y0 + 6.0 => ay - hud_h - 12.0,
            Some(ay) => ay + 30.0,
            None => body_y0 + 12.0,
        };
        let hud = Rect::new(hud_x, hud_y, hud_x + hud_w, hud_y + hud_h);

        // Shadow + body.
        let shadow = Rect::new(hud.x0 - 1.0, hud.y0 + 3.0, hud.x1 + 1.0, hud.y1 + 5.0);
        paint::fill_round(scene, t, shadow, hud_h / 2.0, Color::from_rgba8(0, 0, 0, 70));
        paint::fill_round(scene, t, hud, hud_h / 2.0, c.surfaces.raised);
        paint::stroke_round(scene, t, hud, hud_h / 2.0, c.border_standard, 1.0);
        // Swallow taps on the bar body so they don't fall through to the page.
        self.push_hit(hud, Action::Swallow);

        let cyc = hud_y + hud_h / 2.0;
        let mut x = hud_x + pad + dot_r;
        for col in HiColor::ALL {
            let center = Point::new(x, cyc);
            let rect = Rect::new(center.x - dot_r, center.y - dot_r, center.x + dot_r, center.y + dot_r);
            paint::circle(scene, t, center, dot_r, ReaderPalette::highlight_swatch(col));
            if self.hi_color == col {
                paint::stroke_round(scene, t, rect.inflate(2.5, 2.5), dot_r + 2.5, theme.text_primary, 1.8);
            } else if self.is_hover(rect) {
                paint::stroke_round(scene, t, rect.inflate(2.5, 2.5), dot_r + 2.5, c.border_standard, 1.2);
            }
            self.push_hit(rect.inflate(3.0, 3.0), Action::SetColor(col));
            x += dot_r * 2.0 + dot_gap;
        }
        // Divider between the swatches and the Note action.
        let dots_end = hud_x + pad + dots_w;
        let div_x = dots_end + sep_w / 2.0;
        paint::line(scene, t, Point::new(div_x, cyc - 10.0), Point::new(div_x, cyc + 10.0), c.border_standard, 1.0);
        // Note pill.
        let note_x = dots_end + sep_w;
        let note_rect = Rect::new(note_x, cyc - 14.0, note_x + note_w, cyc + 14.0);
        let note_hover = self.is_hover(note_rect);
        paint::fill_round(scene, t, note_rect, 14.0, if note_hover { theme.bg_hover } else { c.surfaces.overlay });
        tr.draw_text_weighted(
            scene,
            "Note",
            13.0,
            c.text_primary,
            t * Affine::translate((note_x + 12.0, cyc - 7.0)),
            None,
            TextWeight::Medium,
        );
        self.push_hit(note_rect, Action::AddNote);
    }

    /// The on-page note composer: a small card anchored near the selected
    /// passage (device-size permitting), so writing a note stays in context on
    /// the page rather than jumping to a separate screen. Typing is handled by
    /// the composing key path; this draws the surface, the live text with a
    /// caret, and Save / Delete. A tap outside the card saves and closes.
    fn draw_note_composer(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        anchor_y: Option<f64>,
    ) {
        let Some(id) = self.composing else { return };
        if self.view != View::Reader {
            return;
        }
        let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        let r = self.rect;

        // Tap-away saves and closes (no dimming — the page stays the preview).
        self.push_hit(r, Action::FinishNote);

        let cw = 344.0_f64.min(r.width() - 24.0);
        let pad = 16.0;
        let inner_w = (cw - pad * 2.0) as f32;
        let excerpt = if a.excerpt.trim().is_empty() {
            "Selected passage".to_string()
        } else {
            format!("\u{201C}{}\u{201D}", a.excerpt)
        };
        let (_, eh) = tr.measure(&excerpt, 12.0, Some(inner_w));
        let note_disp = format!("{}\u{2502}", a.note);
        let (_, nh) = tr.measure(&note_disp, 13.5, Some(inner_w));
        let note_h = (nh as f64).max(20.0);
        let ch = 34.0 + eh as f64 + 10.0 + note_h + 16.0 + 40.0 + pad;

        let cx = (r.x0 + r.x1) / 2.0;
        let x0 = (cx - cw / 2.0).clamp(r.x0 + 12.0, (r.x1 - 12.0 - cw).max(r.x0 + 12.0));
        let top_lim = r.y0 + TOP_BAR + 8.0;
        let bot_lim = r.y1 - BOTTOM_BAR - 8.0;
        let y0 = match anchor_y {
            Some(ay) if ay + 22.0 + ch <= bot_lim => ay + 22.0,
            Some(ay) if ay - 14.0 - ch >= top_lim => ay - 14.0 - ch,
            _ => (bot_lim - ch).max(top_lim),
        };
        let card = Rect::new(x0, y0, x0 + cw, y0 + ch);
        let shadow = Rect::new(card.x0 - 1.0, card.y0 + 4.0, card.x1 + 1.0, card.y1 + 7.0);
        paint::fill_round(scene, t, shadow, 12.0, Color::from_rgba8(0, 0, 0, 80));
        paint::fill_round(scene, t, card, theme.tokens.radii.lg as f64, c.surfaces.raised);
        paint::stroke_round(scene, t, card, theme.tokens.radii.lg as f64, theme.border_focus, 1.4);
        self.push_hit(card, Action::Swallow);

        let ix0 = x0 + pad;
        let mut y = y0 + pad;
        // Header: colour dot + "Note".
        paint::circle(scene, t, Point::new(ix0 + 6.0, y + 7.0), 6.0, ReaderPalette::highlight_swatch(a.color));
        tr.draw_text_weighted(scene, "Note", 13.5, c.text_primary, t * Affine::translate((ix0 + 20.0, y)), None, TextWeight::Semibold);
        y += 26.0;
        // Excerpt.
        tr.draw_text(scene, &excerpt, 12.0, c.text_dim, t * Affine::translate((ix0, y)), Some(inner_w));
        y += eh as f64 + 10.0;
        // Live note text with caret.
        tr.draw_text(scene, &note_disp, 13.5, c.text_primary, t * Affine::translate((ix0, y)), Some(inner_w));
        y += note_h + 16.0;
        // Delete + Save.
        let del_w = tr.measure_weighted("Delete", 12.5, None, TextWeight::Medium).0 as f64 + 24.0;
        let del = Rect::new(ix0, y, ix0 + del_w, y + 32.0);
        let del_hover = self.is_hover(del);
        paint::fill_round(scene, t, del, 16.0, if del_hover { theme.bg_hover } else { c.surfaces.overlay });
        tr.draw_text_weighted(scene, "Delete", 12.5, c.semantic.danger, t * Affine::translate((ix0 + 12.0, y + 8.0)), None, TextWeight::Medium);
        self.push_hit(del, Action::Delete(id));

        let save_w = tr.measure_weighted("Save", 13.0, None, TextWeight::Semibold).0 as f64 + 40.0;
        let save = Rect::new(x0 + cw - pad - save_w, y, x0 + cw - pad, y + 32.0);
        paint::fill_round(scene, t, save, 16.0, theme.accent_primary);
        tr.draw_text_weighted(scene, "Save", 13.0, Color::from_rgb8(248, 248, 252), t * Affine::translate((save.x0 + 20.0, y + 8.0)), None, TextWeight::Semibold);
        self.push_hit(save, Action::FinishNote);
    }

    /// Paint the contents + annotations rails. Each rail is either docked
    /// (flush against its edge, the page already inset to make room) or an
    /// overlay flap that slides in over a full-width page behind a single
    /// dimming scrim. The shared [`FlapState`] kernel supplies the eased
    /// `reveal` (scrim alpha + docked inset) and `slide` (overlay offset).
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
            self.push_hit(Rect::new(x0, body_y0, x0 + NOTES_W, body_y1), Action::Swallow);
            self.draw_notes(scene, t, tr, theme, x0, body_y0, body_y1, c);
        }
    }

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
        let mut x = r.x0 + 14.0;
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::Back, false, Action::Back) + 6.0;
        let toc_active = self.toc_open();
        self.icon_button(scene, t, theme, x, cy, s, Ico::Toc, toc_active, Action::ToggleToc);

        // centre: title (+ author, when it fits). The title is clamped to the
        // gap between the left controls and the right controls and ellipsised
        // rather than allowed to collide with either cluster on a narrow window.
        if let Some(book) = lib.get(self.book_idx) {
            let left_bound = x + s + 12.0;
            let right_bound = r.x1 - 14.0 - s - 4.0 - s_bm - 12.0;
            let avail = (right_bound - left_bound).max(40.0);

            let auth = format!("   {}", book.author);
            let (aw, _) = tr.measure(&auth, 13.0, None);
            let full_title = ellipsize(tr, &book.title, 16.0, TextWeight::Semibold, avail);
            let (tw, _) = tr.measure_weighted(&full_title, 16.0, None, TextWeight::Semibold);
            // Only show the author if title + author both fit comfortably.
            let show_author = tw as f64 + aw as f64 <= avail && book.title == full_title;
            let total = if show_author { tw as f64 + aw as f64 } else { tw as f64 };
            let cx = ((left_bound + right_bound) / 2.0 - total / 2.0).max(left_bound);
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
                    t * Affine::translate((cx + tw as f64, r.y0 + 18.5)),
                    None,
                );
            }
        }

        // right: bookmark toggle + notes toggle
        let bookmarked = self.annotations.iter().any(|a| {
            a.kind == AnnoKind::Bookmark && a.chapter == self.chapter && a.block == self.page_top_block
        });
        let mut rx = r.x1 - 14.0 - s;
        let notes_active = self.notes_open();
        self.icon_button(scene, t, theme, rx, cy, s, Ico::Notes, notes_active, Action::ToggleNotes);
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
    }

    /// The bottom bar carries only page navigation and one "Aa" button that
    /// opens the reading-settings popover. Keeping the type/spacing/theme
    /// controls out of the always-on chrome is what lets the bar stay clean and
    /// non-overflowing from 1280 all the way down to 560px.
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_bottombar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
        nchap: usize,
    ) {
        let r = self.rect;
        let y0 = r.y1 - BOTTOM_BAR;
        let bar = Rect::new(r.x0, y0, r.x1, r.y1);
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::line(scene, t, Point::new(r.x0, y0), Point::new(r.x1, y0), c.border_standard, 1.0);

        let s = 34.0;
        let cy = y0 + (BOTTOM_BAR - s) / 2.0;
        let mut x = r.x0 + 18.0;

        // --- left: page navigation ---------------------------------------
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::Prev, false, Action::PrevPage) + 2.0;
        let pg = format!("{} / {}", self.page + 1, self.cur_page_count.max(1));
        let (pw, _) = tr.measure_weighted(&pg, 13.0, None, TextWeight::Medium);
        tr.draw_text_weighted(
            scene,
            &pg,
            13.0,
            c.text_primary,
            t * Affine::translate((x + 6.0, cy + 8.0)),
            None,
            TextWeight::Medium,
        );
        x += pw as f64 + 12.0;
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::Next, false, Action::NextPage);

        // Chapter locator, only when there is comfortable room for it.
        let loc = format!("Chapter {} of {}", self.chapter + 1, nchap.max(1));
        let (lw, _) = tr.measure(&loc, 12.0, None);

        // --- right: reading-settings ("Aa") ------------------------------
        let pill_h = 30.0;
        let pill_y = y0 + (BOTTOM_BAR - pill_h) / 2.0;
        let (aw, _) = tr.measure_weighted("Aa", 15.0, None, TextWeight::Semibold);
        let aa_w = aw as f64 + 26.0;
        let aa_x = r.x1 - 18.0 - aa_w;
        let aa_rect = Rect::new(aa_x, pill_y, aa_x + aa_w, pill_y + pill_h);
        let aa_active = self.type_popover;
        let aa_hover = self.is_hover(aa_rect);
        let aa_bg = if aa_active {
            theme.accent_primary
        } else if aa_hover {
            theme.bg_hover
        } else {
            c.surfaces.overlay
        };
        paint::fill_round(scene, t, aa_rect, pill_h / 2.0, aa_bg);
        let aa_ink = if aa_active { Color::from_rgb8(248, 248, 252) } else { c.text_primary };
        tr.draw_text_weighted(
            scene,
            "Aa",
            15.0,
            aa_ink,
            t * Affine::translate((aa_x + 13.0, pill_y + 6.0)),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(aa_rect, Action::ToggleTypePopover);

        // Draw the chapter locator centred in the gap between the nav cluster
        // and the Aa button, but only if it fits without crowding either.
        let gap_start = x + 20.0;
        let gap_end = aa_x - 20.0;
        if gap_end - gap_start > lw as f64 {
            let lx = (gap_start + gap_end) / 2.0 - lw as f64 / 2.0;
            tr.draw_text(scene, &loc, 12.0, c.text_dim, t * Affine::translate((lx, cy + 9.0)), None);
        }
    }

    /// The reading-settings popover summoned by the bottom bar's "Aa" button:
    /// text size, line spacing, typeface, and page tint. A full-surface scrim
    /// behind it makes any tap-away dismiss it. Drawn last so it floats on top.
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

        let pad = 16.0;
        let row_h = 34.0;
        let pill_h = row_h - 4.0;
        let pill_gap = 8.0;
        let panel_w = 320.0_f64.min(r.width() - 24.0);
        let inner_w = panel_w - pad * 2.0;

        // Pre-lay the quick-settings font pills so the panel sizes to however
        // many fonts the user has chosen to show.
        let shown = self.shown_fonts();
        let active_id = self.active_font().id;
        let font_pills: Vec<(String, String, f64, bool)> = shown
            .iter()
            .map(|f| {
                let w =
                    tr.measure_weighted(&f.name, 12.5, None, TextWeight::Medium).0 as f64 + 24.0;
                (f.id.clone(), f.name.clone(), w, f.id == active_id)
            })
            .collect();
        let font_widths: Vec<f64> = font_pills.iter().map(|p| p.2).collect();
        let font_rows = wrap_row_count(&font_widths, inner_w, pill_gap);

        // Enabled colour schemes offered as PAGE pills.
        let scheme_pills: Vec<SchemePill> = self.quick_scheme_pills();
        let scheme_widths: Vec<f64> = scheme_pills
            .iter()
            .map(|p| tr.measure_weighted(&p.label, 12.5, None, TextWeight::Medium).0 as f64 + 24.0)
            .collect();
        let scheme_rows = wrap_row_count(&scheme_widths, inner_w, pill_gap);

        let steppers_h = row_h + 10.0 + row_h;
        let type_h =
            16.0 + font_rows as f64 * pill_h + font_rows.saturating_sub(1) as f64 * pill_gap;
        let page_h =
            16.0 + scheme_rows as f64 * pill_h + scheme_rows.saturating_sub(1) as f64 * pill_gap;
        let manage_h = 16.0 + pill_h;
        let panel_h = pad * 2.0 + steppers_h + 22.0 + type_h + 22.0 + page_h + 16.0 + manage_h;

        let bar_top = r.y1 - BOTTOM_BAR;
        let px1 = r.x1 - 18.0;
        let px0 = (px1 - panel_w).max(r.x0 + 12.0);
        let py1 = bar_top - 10.0;
        let py0 = (py1 - panel_h).max(r.y0 + TOP_BAR + 8.0);
        let panel = Rect::new(px0, py0, px1, py1);
        // Body panel (its own tap-swallow so clicks inside don't dismiss).
        paint::fill_round(scene, t, panel, theme.tokens.radii.lg as f64, c.surfaces.raised);
        paint::stroke_round(scene, t, panel, theme.tokens.radii.lg as f64, c.border_standard, 1.0);
        self.push_hit(panel, Action::Swallow);

        let ix0 = px0 + pad;
        let ix1 = px1 - pad;
        let mut y = py0 + pad;

        // Text size stepper.
        let fs = format!("{}%", (self.settings.font_scale * 100.0).round() as i32);
        self.stepper_row(scene, t, tr, theme, c, ix0, ix1, y, row_h, "Text size", &fs, Action::FontDec, Action::FontInc);
        y += row_h + 10.0;
        // Line spacing stepper.
        let ls = format!("{:.1}", self.settings.line_spacing);
        self.stepper_row(scene, t, tr, theme, c, ix0, ix1, y, row_h, "Line spacing", &ls, Action::SpacingDec, Action::SpacingInc);
        y += row_h + 22.0;

        // Typeface — one pill per shown font, wrapping within the panel.
        tr.draw_text_weighted(scene, "TYPEFACE", 10.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 16.0;
        let mut sx = ix0;
        for (id, name, w, active) in &font_pills {
            if sx + w > ix1 && sx > ix0 {
                sx = ix0;
                y += pill_h + pill_gap;
            }
            let action = if *active { Action::Swallow } else { Action::SetFont(id.clone()) };
            self.pill(scene, t, tr, theme, sx, y, pill_h, name, 12.5, *active, Some(action));
            sx += w + pill_gap;
        }
        y += pill_h + 22.0;

        // Page — one pill per enabled colour scheme, wrapping.
        tr.draw_text_weighted(scene, "PAGE", 10.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 16.0;
        let mut sx = ix0;
        for (p, w) in scheme_pills.iter().zip(scheme_widths.iter()) {
            if sx + w > ix1 && sx > ix0 {
                sx = ix0;
                y += pill_h + pill_gap;
            }
            let action = if p.active { Action::Swallow } else { p.action.clone() };
            self.pill(scene, t, tr, theme, sx, y, pill_h, &p.label, 12.5, p.active, Some(action));
            sx += w + pill_gap;
        }
        y += pill_h + 16.0;

        // A quiet link into the full font + colour-scheme managers.
        let manage = "Manage fonts & colours…";
        let (mw, _) = tr.measure_weighted(manage, 12.0, None, TextWeight::Medium);
        let mrect = Rect::new(ix0, y, ix0 + mw as f64 + 22.0, y + pill_h);
        let mhover = self.is_hover(mrect);
        paint::fill_round(scene, t, mrect, pill_h / 2.0, if mhover { theme.bg_hover } else { c.surfaces.overlay });
        tr.draw_text_weighted(
            scene,
            manage,
            12.0,
            c.text_primary,
            t * Affine::translate((ix0 + 12.0, y + (pill_h - 12.0) / 2.0)),
            None,
            TextWeight::Medium,
        );
        self.push_hit(mrect, Action::OpenSettings);
    }

    /// A labelled −/value/＋ stepper row used inside the reading popover.
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
        value: &str,
        dec: Action,
        inc: Action,
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
        let btn = h;
        let plus_x = x1 - btn;
        let (vw, _) = tr.measure_weighted(value, 13.0, None, TextWeight::Medium);
        let value_slot = 46.0_f64;
        let minus_x = plus_x - value_slot - btn;
        self.icon_button(scene, t, theme, minus_x, y, btn, Ico::MinusI, false, dec);
        tr.draw_text_weighted(
            scene,
            value,
            13.0,
            c.text_primary,
            t * Affine::translate((minus_x + btn + (value_slot - vw as f64) / 2.0, y + (h - 13.0) / 2.0)),
            None,
            TextWeight::Medium,
        );
        self.icon_button(scene, t, theme, plus_x, y, btn, Ico::PlusI, false, inc);
    }

    /// `panel_x0` is the rail's left edge — `r.x0` when docked, or a slid
    /// position when it floats as an overlay flap.
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
        paint::line(scene, t, Point::new(panel_x0 + TOC_W, y0), Point::new(panel_x0 + TOC_W, y1), c.border_standard, 1.0);
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
                paint::fill_round(scene, t, row, theme.tokens.radii.md as f64, theme.accent_primary.with_alpha_factor(0.16));
                let bar = Rect::new(row.x0, row.y0 + 6.0, row.x0 + 3.0, row.y1 - 6.0);
                paint::fill_round(scene, t, bar, 2.0, theme.accent_primary);
            } else if hover {
                paint::fill_round(scene, t, row, theme.tokens.radii.md as f64, theme.bg_hover);
            }
            let num = format!("{:02}", i + 1);
            tr.draw_text_weighted(
                scene,
                &num,
                12.0,
                if active { theme.accent_primary } else { c.text_dim },
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
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
            self.push_hit(row, Action::GotoChapter(i));
            y += row_h;
        }
    }

    /// `panel_x0` is the rail's left edge — `r.x1 - NOTES_W` when docked, or a
    /// slid position when it floats as an overlay flap from the right.
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
        paint::line(scene, t, Point::new(x0, y0), Point::new(x0, y1), c.border_standard, 1.0);
        tr.draw_text_weighted(
            scene,
            "ANNOTATIONS",
            11.0,
            c.text_dim,
            t * Affine::translate((x0 + 22.0, y0 + 18.0)),
            None,
            TextWeight::Semibold,
        );
        let count = format!("{}", annos.len());
        let (cw, _) = tr.measure(&count, 11.0, None);
        let badge = Rect::new(x0 + 110.0, y0 + 14.0, x0 + 110.0 + cw as f64 + 16.0, y0 + 14.0 + 18.0);
        paint::fill_round(scene, t, badge, 9.0, c.surfaces.overlay);
        tr.draw_text(scene, &count, 11.0, c.text_dim, t * Affine::translate((x0 + 118.0, y0 + 17.0)), None);

        // action row: "Add note" on the selected block
        let add_y = y0 + 42.0;
        let has_sel = self.selected_block.is_some();
        let add_label = if has_sel { "+ Note on selection" } else { "Select a paragraph to annotate" };
        let add_rect = Rect::new(x0 + 16.0, add_y, panel_x1 - 16.0, add_y + 30.0);
        let hover = has_sel && self.is_hover(add_rect);
        paint::fill_round(
            scene,
            t,
            add_rect,
            theme.tokens.radii.md as f64,
            if hover { theme.bg_hover } else { c.surfaces.overlay },
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

        // list
        let mut y = add_y + 46.0;
        let card_x0 = x0 + 16.0;
        let card_x1 = panel_x1 - 16.0;
        let inner_w = (card_x1 - card_x0 - 28.0) as f32;
        let list = annos.clone();
        for a in &list {
            if y > y1 - 40.0 {
                break;
            }
            // measure note/excerpt to size the card
            let excerpt = if a.excerpt.is_empty() { "(no excerpt)".to_string() } else { format!("“{}”", a.excerpt) };
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
            let card_h = 30.0 + eh as f64 + if note_disp.is_empty() { 0.0 } else { nh as f64 + 12.0 } + 16.0;
            let card = Rect::new(card_x0, y, card_x1, y + card_h);
            let hover = self.is_hover(card);
            paint::fill_round(scene, t, card, theme.tokens.radii.md as f64, c.surfaces.raised);
            if composing {
                paint::stroke_round(scene, t, card, theme.tokens.radii.md as f64, theme.border_focus, 1.6);
            } else if hover {
                paint::stroke_round(scene, t, card, theme.tokens.radii.md as f64, c.border_standard, 1.0);
            }

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
                t * Affine::translate((card_x1 - 30.0 - lw as f64, y + 12.0)),
                None,
            );
            // Delete (×) — always drawn and always registered as a hit, so it is
            // reachable by touch and by keyboard-driven navigation, not only
            // while a pointer happens to hover the card. Hover just strengthens
            // it. This is the only way to remove an annotation, so gating it on
            // hover made deletion impossible without a mouse.
            let del = Rect::new(card_x1 - 26.0, y + 6.0, card_x1 - 6.0, y + 26.0);
            let del_hover = self.is_hover(del);
            let del_col = if del_hover {
                c.text_primary
            } else {
                c.text_dim.with_alpha_factor(0.55)
            };
            paint::close(scene, t, del, del_col, 1.4);
            self.push_hit(del, Action::Delete(a.id));

            // excerpt
            tr.draw_text(
                scene,
                &excerpt,
                12.0,
                c.text_dim,
                t * Affine::translate((card_x0 + 14.0, y + 30.0)),
                Some(inner_w),
            );
            // note body
            if !note_disp.is_empty() {
                tr.draw_text(
                    scene,
                    &note_disp,
                    12.5,
                    c.text_primary,
                    t * Affine::translate((card_x0 + 14.0, y + 30.0 + eh as f64 + 8.0)),
                    Some(inner_w),
                );
            }

            // click card → jump; click again on a note → edit
            let action = if a.kind == AnnoKind::Note {
                Action::EditNote(a.id)
            } else {
                Action::JumpTo(a.id)
            };
            // jump is the primary intent for the whole card area minus delete
            self.push_hit(Rect::new(card_x0, y, card_x1 - 30.0, y + card_h), Action::JumpTo(a.id));
            if a.kind == AnnoKind::Note {
                // a smaller secondary zone to begin editing
                self.push_hit(Rect::new(card_x0, y + 28.0, card_x1 - 30.0, y + card_h), action);
            }

            y += card_h + 12.0;
        }
    }
}

// ==========================================================================
// Settings screen — the font + colour-scheme managers
// ==========================================================================

impl BooksApp {
    fn draw_settings(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = theme.tokens.colors.clone();
        let r = self.rect;

        // Top bar.
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::line(scene, t, Point::new(r.x0, r.y0 + TOP_BAR), Point::new(r.x1, r.y0 + TOP_BAR), c.border_standard, 1.0);
        let s = 34.0;
        let cy = r.y0 + (TOP_BAR - s) / 2.0;
        self.icon_button(scene, t, theme, r.x0 + 14.0, cy, s, Ico::Back, false, Action::CloseSettings);
        tr.draw_text_weighted(scene, "Settings", 21.0, c.text_primary, t * Affine::translate((r.x0 + 60.0, r.y0 + 15.0)), None, TextWeight::Bold);

        // Tab switch.
        let tabs_y = r.y0 + TOP_BAR + 14.0;
        let mut tx = r.x0 + 24.0;
        for (tab, label) in [(SettingsTab::Fonts, "Fonts"), (SettingsTab::Colours, "Colours")] {
            let active = self.settings_tab == tab;
            let action = if active { Action::Swallow } else { Action::SetSettingsTab(tab) };
            tx += self.pill(scene, t, tr, theme, tx, tabs_y, 32.0, label, 13.5, active, Some(action)) + 10.0;
        }

        // Scrollable body.
        let content_top = tabs_y + 32.0 + 16.0;
        let body = Rect::new(r.x0, content_top, r.x1, r.y1);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &body);
        let col_w = (r.width() - 96.0).min(760.0).max(280.0);
        let col_x = r.x0 + (r.width() - col_w) / 2.0;
        let start_y = content_top - self.settings_scroll;
        let end_y = match self.settings_tab {
            SettingsTab::Fonts => self.draw_fonts_manager(scene, t, tr, theme, &c, col_x, col_w, start_y),
            SettingsTab::Colours => self.draw_schemes_manager(scene, t, tr, theme, &c, col_x, col_w, start_y),
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
    }

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
        mut y: f64,
    ) -> f64 {
        let radii_md = theme.tokens.radii.md as f64;
        tr.draw_text_weighted(scene, "READING FONTS", 11.0, c.text_dim, t * Affine::translate((x, y)), None, TextWeight::Semibold);
        y += 20.0;
        tr.draw_text(scene, "Pick the default, tune each face's size, and choose which appear in the quick settings.", 12.5, c.text_dim, t * Affine::translate((x, y)), Some(w as f32));
        y += 30.0;

        let fonts = self.fonts.clone();
        let default_id = if self.default_font_id.is_empty() {
            fonts.first().map(|f| f.id.clone()).unwrap_or_default()
        } else {
            self.default_font_id.clone()
        };
        for f in &fonts {
            let row_h = 64.0;
            let card = Rect::new(x, y, x + w, y + row_h);
            paint::fill_round(scene, t, card, radii_md, c.surfaces.raised);

            // Default radio.
            let rc = Point::new(x + 26.0, y + row_h / 2.0);
            let is_def = f.id == default_id;
            let ring = Rect::new(rc.x - 8.0, rc.y - 8.0, rc.x + 8.0, rc.y + 8.0);
            if is_def {
                paint::circle(scene, t, rc, 8.0, theme.accent_primary);
                paint::circle(scene, t, rc, 3.4, Color::from_rgb8(248, 248, 252));
            } else {
                paint::stroke_round(scene, t, ring, 8.0, c.border_standard, 1.4);
            }
            self.push_hit(Rect::new(x + 8.0, y, x + 46.0, y + row_h), Action::SetDefaultFont(f.id.clone()));

            // Name (tap to rename) + kind sub-label.
            let name_x = x + 52.0;
            let renaming_this = matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Font(f.id.clone()));
            let name_disp = if renaming_this {
                format!("{}\u{2502}", self.renaming.as_ref().map(|r| r.buf.as_str()).unwrap_or(""))
            } else {
                f.name.clone()
            };
            tr.draw_text_weighted(scene, &name_disp, 15.0, c.text_primary, t * Affine::translate((name_x, y + 13.0)), None, TextWeight::Semibold);
            let kind = match &f.kind {
                FontKind::Default => "Default face".to_string(),
                FontKind::Serif => "Serif".to_string(),
                FontKind::Mono => "Monospace".to_string(),
                FontKind::Named(n) => format!("System · {n}"),
            };
            tr.draw_text(scene, &kind, 12.0, c.text_dim, t * Affine::translate((name_x, y + 36.0)), None);
            self.push_hit(Rect::new(name_x, y + 8.0, x + w - 280.0, y + 34.0), Action::BeginRenameFont(f.id.clone()));

            // Right cluster: shown toggle, size stepper, remove (custom only).
            let mut rx = x + w - 16.0;
            if matches!(f.kind, FontKind::Named(_)) {
                rx -= 24.0;
                let del = Rect::new(rx, y + row_h / 2.0 - 12.0, rx + 24.0, y + row_h / 2.0 + 12.0);
                let col = if self.is_hover(del) { c.text_primary } else { c.text_dim.with_alpha_factor(0.6) };
                paint::close(scene, t, del, col, 1.4);
                self.push_hit(del, Action::RemoveFont(f.id.clone()));
                rx -= 10.0;
            }
            let btn = 28.0;
            let by = y + row_h / 2.0 - btn / 2.0;
            rx -= btn;
            self.icon_button(scene, t, theme, rx, by, btn, Ico::PlusI, false, Action::FontMultInc(f.id.clone()));
            let mult = format!("{:.2}×", f.size_mult);
            let (vw, _) = tr.measure_weighted(&mult, 12.5, None, TextWeight::Medium);
            rx -= 6.0 + vw as f64;
            tr.draw_text_weighted(scene, &mult, 12.5, c.text_primary, t * Affine::translate((rx, y + row_h / 2.0 - 7.0)), None, TextWeight::Medium);
            rx -= 6.0 + btn;
            self.icon_button(scene, t, theme, rx, by, btn, Ico::MinusI, false, Action::FontMultDec(f.id.clone()));
            rx -= 12.0;
            let label = if f.shown { "In quick settings" } else { "Hidden" };
            let tw = tr.measure_weighted(label, 12.0, None, TextWeight::Medium).0 as f64 + 24.0;
            rx -= tw;
            self.pill(scene, t, tr, theme, rx, y + row_h / 2.0 - 13.0, 26.0, label, 12.0, f.shown, Some(Action::ToggleFontShown(f.id.clone())));

            y += row_h + 10.0;
        }

        // Add a system font.
        let add = "＋  Add a system font";
        let aw = tr.measure_weighted(add, 13.0, None, TextWeight::Medium).0 as f64 + 32.0;
        let arect = Rect::new(x, y, x + aw, y + 36.0);
        paint::fill_round(scene, t, arect, 18.0, if self.is_hover(arect) { theme.bg_hover } else { c.surfaces.overlay });
        paint::stroke_round(scene, t, arect, 18.0, c.border_standard, 1.0);
        tr.draw_text_weighted(scene, add, 13.0, c.text_primary, t * Affine::translate((x + 16.0, y + 11.0)), None, TextWeight::Medium);
        self.push_hit(arect, Action::OpenFontPicker);
        y + 36.0 + 8.0
    }

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
        mut y: f64,
    ) -> f64 {
        let radii_md = theme.tokens.radii.md as f64;
        tr.draw_text_weighted(scene, "COLOUR SCHEMES", 11.0, c.text_dim, t * Affine::translate((x, y)), None, TextWeight::Semibold);
        y += 20.0;
        tr.draw_text(scene, "Duplicate a preset or build one from a background colour, then tune each colour. Disabled schemes are hidden from the quick settings.", 12.5, c.text_dim, t * Affine::translate((x, y)), Some(w as f32));
        y += 42.0;

        // New from a background colour.
        tr.draw_text_weighted(scene, "NEW FROM A COLOUR", 10.0, c.text_dim, t * Affine::translate((x, y)), None, TextWeight::Semibold);
        y += 16.0;
        let sw = 34.0;
        let mut sx = x;
        for bg in BG_SWATCHES {
            let sr = Rect::new(sx, y, sx + sw, y + sw);
            paint::fill_round(scene, t, sr, 8.0, Color::from_rgb8(bg[0], bg[1], bg[2]));
            let hov = self.is_hover(sr);
            paint::stroke_round(scene, t, sr, 8.0, if hov { theme.border_focus } else { c.border_standard }, if hov { 2.0 } else { 1.0 });
            self.push_hit(sr, Action::NewSchemeFromBg(bg));
            sx += sw + 10.0;
        }
        y += sw + 24.0;

        let schemes = self.schemes.clone();
        for s in &schemes {
            let row_h = 66.0;
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

            // Swatches (page / ink / accent).
            let mut swx = x + 18.0;
            for col in [s.page, s.ink, s.accent] {
                let srr = Rect::new(swx, y + row_h / 2.0 - 11.0, swx + 22.0, y + row_h / 2.0 + 11.0);
                paint::fill_round(scene, t, srr, 5.0, Color::from_rgb8(col[0], col[1], col[2]));
                paint::stroke_round(scene, t, srr, 5.0, c.border_standard, 1.0);
                swx += 26.0;
            }
            let use_action = if s.builtin {
                Action::SetReadTheme(ReadTheme::from_str(&s.id))
            } else {
                Action::SetScheme(s.id.clone())
            };
            self.push_hit(Rect::new(x + 8.0, y + 6.0, swx + 4.0, y + row_h - 6.0), use_action.clone());

            // Name + status.
            let name_x = swx + 14.0;
            let renaming_this = matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Scheme(s.id.clone()));
            let name_disp = if renaming_this {
                format!("{}\u{2502}", self.renaming.as_ref().map(|r| r.buf.as_str()).unwrap_or(""))
            } else {
                s.name.clone()
            };
            tr.draw_text_weighted(scene, &name_disp, 15.0, c.text_primary, t * Affine::translate((name_x, y + 13.0)), None, TextWeight::Semibold);
            let sub = if s.builtin { "Built-in" } else if selected { "In use" } else { "Custom" };
            tr.draw_text(scene, sub, 12.0, c.text_dim, t * Affine::translate((name_x, y + 36.0)), None);
            if s.builtin {
                self.push_hit(Rect::new(name_x, y + 6.0, x + w - 300.0, y + row_h - 6.0), use_action);
            } else {
                self.push_hit(Rect::new(name_x, y + 6.0, x + w - 300.0, y + 34.0), Action::BeginRenameScheme(s.id.clone()));
            }

            // Right cluster: enable, Duplicate, Edit(custom), Delete(custom).
            let mut rx = x + w - 16.0;
            if !s.builtin {
                rx -= 24.0;
                let del = Rect::new(rx, y + row_h / 2.0 - 12.0, rx + 24.0, y + row_h / 2.0 + 12.0);
                let col = if self.is_hover(del) { c.text_primary } else { c.text_dim.with_alpha_factor(0.6) };
                paint::close(scene, t, del, col, 1.4);
                self.push_hit(del, Action::DeleteScheme(s.id.clone()));
                rx -= 10.0;
                let ew = tr.measure_weighted("Edit", 12.0, None, TextWeight::Medium).0 as f64 + 24.0;
                rx -= ew;
                self.pill(scene, t, tr, theme, rx, y + row_h / 2.0 - 13.0, 26.0, "Edit", 12.0, false, Some(Action::EditScheme(s.id.clone())));
                rx -= 8.0;
            }
            let dw = tr.measure_weighted("Duplicate", 12.0, None, TextWeight::Medium).0 as f64 + 24.0;
            rx -= dw;
            self.pill(scene, t, tr, theme, rx, y + row_h / 2.0 - 13.0, 26.0, "Duplicate", 12.0, false, Some(Action::DuplicateScheme(s.id.clone())));
            rx -= 8.0;
            let tl = if s.enabled { "Enabled" } else { "Disabled" };
            let tw = tr.measure_weighted(tl, 12.0, None, TextWeight::Medium).0 as f64 + 24.0;
            rx -= tw;
            self.pill(scene, t, tr, theme, rx, y + row_h / 2.0 - 13.0, 26.0, tl, 12.0, s.enabled, Some(Action::ToggleSchemeEnabled(s.id.clone())));

            y += row_h + 10.0;
        }
        y + 8.0
    }

    fn draw_scheme_editor(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, c: &ui::tokens::colors::Colors) {
        let Some(id) = self.editing_scheme.clone() else { return };
        let Some(scheme) = self.schemes.iter().find(|s| s.id == id).cloned() else { return };
        let r = self.rect;
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.4);
        scene.fill(Fill::NonZero, t, scrim, None, &r);
        self.push_hit(r, Action::CloseSchemeEditor);

        let pw = 440.0_f64.min(r.width() - 40.0);
        let ph = 372.0_f64.min(r.height() - 40.0);
        let px0 = r.x0 + (r.width() - pw) / 2.0;
        let py0 = r.y0 + (r.height() - ph) / 2.0;
        let panel = Rect::new(px0, py0, px0 + pw, py0 + ph);
        paint::fill_round(scene, t, panel, theme.tokens.radii.lg as f64, c.surfaces.raised);
        paint::stroke_round(scene, t, panel, theme.tokens.radii.lg as f64, c.border_standard, 1.0);
        self.push_hit(panel, Action::Swallow);

        let ix0 = px0 + 22.0;
        let ix1 = px0 + pw - 22.0;
        let mut y = py0 + 20.0;
        tr.draw_text_weighted(scene, "EDIT SCHEME", 11.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 18.0;
        let renaming_this = matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Scheme(id.clone()));
        let name_disp = if renaming_this {
            format!("{}\u{2502}", self.renaming.as_ref().map(|r| r.buf.as_str()).unwrap_or(""))
        } else {
            scheme.name.clone()
        };
        tr.draw_text_weighted(scene, &name_disp, 20.0, c.text_primary, t * Affine::translate((ix0, y)), None, TextWeight::Bold);
        self.push_hit(Rect::new(ix0, y - 2.0, ix1 - 60.0, y + 26.0), Action::BeginRenameScheme(id.clone()));
        y += 40.0;

        tr.draw_text_weighted(scene, "COLOURS · tap a swatch to change", 10.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 18.0;
        let slots = scheme.slots();
        let slot_w = (ix1 - ix0 - 2.0 * 16.0) / 3.0;
        let mut sxx = ix0;
        for (i, (label, col)) in slots.iter().enumerate() {
            let sr = Rect::new(sxx, y, sxx + slot_w, y + 60.0);
            paint::fill_round(scene, t, sr, 10.0, Color::from_rgb8(col[0], col[1], col[2]));
            paint::stroke_round(scene, t, sr, 10.0, c.border_standard, 1.0);
            self.push_hit(sr, Action::CycleSchemeColor(id.clone(), i));
            let (lw, _) = tr.measure(label, 12.0, None);
            tr.draw_text(scene, label, 12.0, c.text_dim, t * Affine::translate((sxx + (slot_w - lw as f64) / 2.0, y + 68.0)), None);
            sxx += slot_w + 16.0;
        }
        y += 60.0 + 32.0;

        tr.draw_text_weighted(scene, "PREVIEW", 10.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 16.0;
        let pal = ReaderPalette::from_scheme(&scheme);
        let prev = Rect::new(ix0, y, ix1, y + 72.0);
        paint::fill_round(scene, t, prev, 8.0, pal.page);
        paint::stroke_round(scene, t, prev, 8.0, pal.page_edge, 1.0);
        tr.draw_text_weighted(scene, "The quick brown fox", 15.0, pal.ink, t * Affine::translate((ix0 + 14.0, y + 12.0)), None, TextWeight::Semibold);
        tr.draw_text(scene, "jumps over the lazy dog.", 13.0, pal.ink_dim, t * Affine::translate((ix0 + 14.0, y + 38.0)), None);
        let chip = Rect::new(ix1 - 64.0, y + 46.0, ix1 - 14.0, y + 54.0);
        paint::fill_round(scene, t, chip, 4.0, pal.accent);
        y += 72.0 + 22.0;

        let done_w = tr.measure_weighted("Done", 13.0, None, TextWeight::Semibold).0 as f64 + 40.0;
        let drect = Rect::new(ix1 - done_w, y, ix1, y + 34.0);
        paint::fill_round(scene, t, drect, 17.0, theme.accent_primary);
        tr.draw_text_weighted(scene, "Done", 13.0, Color::from_rgb8(248, 248, 252), t * Affine::translate((ix1 - done_w + 20.0, y + 9.0)), None, TextWeight::Semibold);
        self.push_hit(drect, Action::CloseSchemeEditor);
    }

    fn draw_font_picker(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, c: &ui::tokens::colors::Colors) {
        let r = self.rect;
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.4);
        scene.fill(Fill::NonZero, t, scrim, None, &r);
        self.push_hit(r, Action::CloseFontPicker);

        let pw = 380.0_f64.min(r.width() - 40.0);
        let ph = (r.height() - 120.0).min(520.0).max(200.0);
        let px0 = r.x0 + (r.width() - pw) / 2.0;
        let py0 = r.y0 + (r.height() - ph) / 2.0;
        let panel = Rect::new(px0, py0, px0 + pw, py0 + ph);
        paint::fill_round(scene, t, panel, theme.tokens.radii.lg as f64, c.surfaces.raised);
        paint::stroke_round(scene, t, panel, theme.tokens.radii.lg as f64, c.border_standard, 1.0);
        self.push_hit(panel, Action::Swallow);
        tr.draw_text_weighted(scene, "Add a system font", 16.0, c.text_primary, t * Affine::translate((px0 + 20.0, py0 + 16.0)), None, TextWeight::Bold);
        let close = Rect::new(px0 + pw - 40.0, py0 + 14.0, px0 + pw - 16.0, py0 + 38.0);
        paint::close(scene, t, close, if self.is_hover(close) { c.text_primary } else { c.text_dim }, 1.5);
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
                paint::fill_round(scene, t, rr, theme.tokens.radii.md as f64, theme.bg_hover);
            }
            // Preview each family by rendering its own name in it.
            tr.draw_text_font(scene, fam, 15.0, c.text_primary, t * Affine::translate((px0 + 22.0, fy + 10.0)), None, TextWeight::Regular, false, &FontChoice::Named(fam.clone()));
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

/// A curated colour ramp the scheme editor cycles a colour slot through. Spans
/// papers, inks, and accent hues so tapping a slot walks a useful set.
const COLOR_RAMP: [[u8; 3]; 16] = [
    [0xFC, 0xFC, 0xFA], // near-white
    [0xF6, 0xEC, 0xD8], // cream
    [0xEC, 0xE7, 0xDA], // warm grey
    [0xD8, 0xE6, 0xDC], // sage
    [0x22, 0x20, 0x1E], // near-black
    [0x1D, 0x1B, 0x18], // warm charcoal
    [0x14, 0x18, 0x22], // navy-black
    [0x20, 0x18, 0x22], // plum-black
    [0xE9, 0xE3, 0xD7], // warm off-white
    [0x8A, 0x76, 0x59], // muted brown
    [0x3B, 0x82, 0xF6], // blue
    [0x0E, 0xA5, 0xE9], // sky
    [0x10, 0xB9, 0x81], // emerald
    [0xF5, 0x9E, 0x0B], // amber
    [0xEF, 0x44, 0x44], // red
    [0xA7, 0x8B, 0xFA], // violet
];

/// Squared RGB distance between two colours.
fn color_dist(a: [u8; 3], b: [u8; 3]) -> i32 {
    let d = |i: usize| (a[i] as i32 - b[i] as i32);
    d(0) * d(0) + d(1) * d(1) + d(2) * d(2)
}

/// Advance a colour to the next entry in [`COLOR_RAMP`], starting from whichever
/// ramp entry is nearest the current colour. Lets the editor "edit" any slot by
/// tapping through the ramp even when the current colour is a derived one.
fn next_ramp_color(cur: [u8; 3]) -> [u8; 3] {
    let idx = COLOR_RAMP
        .iter()
        .enumerate()
        .min_by_key(|(_, c)| color_dist(**c, cur))
        .map(|(i, _)| i)
        .unwrap_or(0);
    COLOR_RAMP[(idx + 1) % COLOR_RAMP.len()]
}

/// Background colours offered by "New from a colour" — a spread of paper and
/// dark-mode bases from which a whole scheme is auto-derived.
const BG_SWATCHES: [[u8; 3]; 8] = [
    [0xFB, 0xF7, 0xEE], // ivory
    [0xF6, 0xEC, 0xD8], // sepia
    [0xEA, 0xEE, 0xF2], // cool paper
    [0xE7, 0xEF, 0xE7], // mint paper
    [0x22, 0x24, 0x2C], // slate
    [0x1B, 0x18, 0x16], // warm dark
    [0x15, 0x1A, 0x24], // midnight
    [0x1E, 0x16, 0x22], // aubergine
];

/// Sub-pixel x-span `[x_lo, x_hi]` of chars `[lo, hi)` within a run laid out at
/// `x0` with measured width `w`, using proportional placement within the run.
fn run_sub_x(x0: f64, w: f64, cs: usize, cl: usize, lo: usize, hi: usize) -> (f64, f64) {
    if cl == 0 {
        return (x0, x0 + w);
    }
    let at = |ch: usize| x0 + (ch.saturating_sub(cs) as f64 / cl as f64) * w;
    (at(lo), at(hi))
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
    if tr.measure_weighted(text, size, None, weight).0 as f64 <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut hi = chars.len();
    // Binary-search the longest prefix that fits with an ellipsis appended.
    let mut best = String::from("…");
    let mut lo = 0;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if tr.measure_weighted(&candidate, size, None, weight).0 as f64 <= max_w {
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
    let iw = img.width.max(1) as f64;
    let ih = img.height.max(1) as f64;
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
    let lum = 0.299 * base.r() as f32 + 0.587 * base.g() as f32 + 0.114 * base.b() as f32;
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
        app.hits.iter().rev().find(|h| pred(&h.action)).map(|h| h.rect.center())
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
        app.handle_event(&mv, 1.0, &mods, doc, &root, &mut handled, &mut focus, node, &taffy);
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        handled = false;
        app.handle_event(&press, 1.0, &mods, doc, &root, &mut handled, &mut focus, node, &taffy);
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
        app.handle_event(&ev, 1.0, &mods, doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
    }

    /// Route a *horizontal* wheel/swipe (`lines` positive == swipe left) through
    /// `handle_event`. This is how a trackpad/touch page-turn swipe arrives.
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
        app.handle_event(&ev, 1.0, &mods, doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
    }

    /// Low-level pointer events for drag-selection tests.
    fn send(app: &mut BooksApp, doc: &mut AutoCommit, ev: WindowEvent) {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(&ev, 1.0, &mods, doc, &root, &mut handled, &mut focus, node, &taffy);
    }
    fn ptr_move(app: &mut BooksApp, doc: &mut AutoCommit, p: Point) {
        send(app, doc, WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position: PhysicalPosition::new(p.x, p.y) });
    }
    fn ptr_down(app: &mut BooksApp, doc: &mut AutoCommit) {
        send(app, doc, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Pressed, button: MouseButton::Left });
    }
    fn ptr_up(app: &mut BooksApp, doc: &mut AutoCommit) {
        send(app, doc, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Released, button: MouseButton::Left });
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

    // Synthesised winit key presses (the zeroed-KeyEvent trick the toolkit's own
    // widget tests use — every field is overwritten before use). Routed through
    // the real `handle_event`, so they exercise the exact production keyboard
    // path the compositor drives.
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
        WindowEvent::KeyboardInput { device_id: DeviceId::dummy(), event: ev, is_synthetic: false }
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
        WindowEvent::KeyboardInput { device_id: DeviceId::dummy(), event: ev, is_synthetic: false }
    }
    fn feed(app: &mut BooksApp, doc: &mut AutoCommit, ev: WindowEvent) -> bool {
        let mods = Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        app.handle_event(&ev, 1.0, &mods, doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
    }
    fn key_char(app: &mut BooksApp, doc: &mut AutoCommit, ch: &str) -> bool {
        feed(app, doc, char_press(ch))
    }
    fn key_named(app: &mut BooksApp, doc: &mut AutoCommit, named: NamedKey) -> bool {
        feed(app, doc, named_press(named))
    }

    #[test]
    fn wheel_turns_pages_in_the_reader() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.view, View::Reader);
        let start = (app.chapter, app.page);
        // A downward flick turns forward.
        assert!(wheel(&mut app, &mut doc, -4.0), "downward wheel turned a page");
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
        assert!((app.chapter, app.page) != mid, "turned back from the advanced spot");
    }

    #[test]
    fn horizontal_swipe_turns_pages() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert_eq!(app.view, View::Reader);
        let start = (app.chapter, app.page);
        // A leftward swipe (positive horizontal travel) turns forward.
        assert!(hwheel(&mut app, &mut doc, 3.0), "swipe left turned a page");
        assert!((app.chapter, app.page) != start, "advanced by swipe");
        // A rightward swipe turns back.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let mid = (app.chapter, app.page);
        hwheel(&mut app, &mut doc, -3.0);
        assert!((app.chapter, app.page) != mid, "swipe right turned back");
    }

    #[test]
    fn selecting_serif_font_applies_and_persists() {
        use crate::model::FontKind;
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        // Default (sans) to start.
        assert_eq!(app.active_font().kind, FontKind::Default);
        // Open the settings popover and pick the Serif font pill — the real path.
        let aa = find_hit(&app, |a| matches!(a, Action::ToggleTypePopover)).expect("Aa button");
        click(&mut app, &mut doc, aa);
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let serif = find_hit(&app, |a| matches!(a, Action::SetFont(id) if id.as_str() == "serif"))
            .expect("serif pill");
        assert!(click(&mut app, &mut doc, serif), "serif pill handled");
        assert_eq!(app.active_font().kind, FontKind::Serif, "serif font is now active");
        assert_eq!(BooksDoc::read_settings(&doc).font_id, "serif", "selection persisted to CRDT");
        // The body face the paint pass will use is serif, not mono.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let (fam, _mult, sig, mono) = app.body_font();
        assert_eq!(fam, ui::text_render::FontChoice::Serif);
        assert!(!mono, "serif is a proportional face");
        let default_sig = font_sig(&crate::model::default_fonts()[0]);
        assert_ne!(sig, default_sig, "font signature differs so the chapter relays out");
    }

    #[test]
    fn disabling_a_scheme_hides_it_from_quick_settings() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 820.0);
        // All three built-in schemes start enabled and offered in quick settings.
        assert_eq!(app.quick_scheme_pills().len(), 3);
        // Toggle the Light scheme off through its manager row.
        let toggle = find_hit(&app, |a| matches!(a, Action::ToggleSchemeEnabled(id) if id.as_str() == "light"))
            .expect("scheme enable toggle");
        assert!(click(&mut app, &mut doc, toggle), "toggle handled");
        frame(&mut app, &mut doc, 1200.0, 820.0);
        let pills = app.quick_scheme_pills();
        assert_eq!(pills.len(), 2, "a disabled scheme drops out of quick settings");
        assert!(!pills.iter().any(|p| p.label == "Light"), "Light is gone from quick settings");
    }

    #[test]
    fn scheme_manager_new_from_bg_creates_selects_and_edits() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Colours;
        frame(&mut app, &mut doc, 1200.0, 860.0);
        let before = BooksDoc::read_schemes(&doc).len();
        // Tap a background swatch to derive a whole new scheme.
        let bg = find_hit(&app, |a| matches!(a, Action::NewSchemeFromBg(_))).expect("a bg swatch");
        assert!(click(&mut app, &mut doc, bg), "new-from-bg handled");
        let schemes = BooksDoc::read_schemes(&doc);
        assert_eq!(schemes.len(), before + 1, "a custom scheme was created");
        let custom = schemes.iter().find(|s| !s.builtin).expect("a custom scheme exists");
        // It becomes the selected scheme and opens in the editor.
        assert_eq!(BooksDoc::read_settings(&doc).scheme_id, custom.id, "new scheme is selected");
        assert_eq!(app.editing_scheme.as_deref(), Some(custom.id.as_str()), "editor opened on it");

        // Duplicating it yields a second custom scheme (schemes ARE duplicable).
        app.editing_scheme = None;
        frame(&mut app, &mut doc, 1200.0, 860.0);
        let dup = find_hit(&app, |a| matches!(a, Action::DuplicateScheme(_))).expect("a duplicate button");
        click(&mut app, &mut doc, dup);
        let after = BooksDoc::read_schemes(&doc);
        assert!(after.iter().filter(|s| !s.builtin).count() >= 2, "duplicate added another custom scheme");
    }

    #[test]
    fn font_manager_toggles_shown_and_sets_default() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        app.view = View::Settings;
        app.settings_tab = SettingsTab::Fonts;
        frame(&mut app, &mut doc, 1200.0, 820.0);
        // Hide the Serif font from quick settings.
        let hide = find_hit(&app, |a| matches!(a, Action::ToggleFontShown(id) if id.as_str() == "serif"))
            .expect("serif shown toggle");
        assert!(click(&mut app, &mut doc, hide), "toggle handled");
        frame(&mut app, &mut doc, 1200.0, 820.0);
        assert!(!app.shown_fonts().iter().any(|f| f.id == "serif"), "serif no longer in quick settings");
        // Make Mono the registry default.
        let def = find_hit(&app, |a| matches!(a, Action::SetDefaultFont(id) if id.as_str() == "mono"))
            .expect("mono default radio");
        assert!(click(&mut app, &mut doc, def), "default handled");
        assert_eq!(BooksDoc::read_default_font(&doc), "mono", "default persisted");
        // With no explicit selection, the active font follows the default.
        assert_eq!(app.active_font().id, "mono", "active font is the new default");
    }

    #[test]
    fn drag_selection_spans_paragraphs_and_highlights_the_range() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Wide window so the docked rails don't cover the page centre.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(app.sel_runs.len() > 3, "the page laid out real run geometry");

        // Press inside the first run, then drag to a run in a *later* block.
        let r0 = app.sel_runs[0];
        let start = Point::new((r0.x0 + r0.x1) / 2.0, (r0.y0 + r0.y1) / 2.0);
        let later = app
            .sel_runs
            .iter()
            .find(|r| r.block > r0.block)
            .cloned()
            .expect("a run in a later paragraph");
        let end = Point::new((later.x0 + later.x1) / 2.0, (later.y0 + later.y1) / 2.0);

        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);

        let sel = app.sel.expect("a drag produced a selection");
        let (s, e) = sel.ordered();
        assert!(e.block > s.block, "the selection spans more than one paragraph");

        // Highlight the selection from the floating toolbar — one ranged
        // highlight per spanned block.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        assert!(click(&mut app, &mut doc, swatch), "swatch handled");
        let hls: Vec<_> = BooksDoc::list_annotations(&doc)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Highlight)
            .collect();
        assert!(hls.len() >= 2, "a ranged highlight per spanned paragraph, got {}", hls.len());
        assert!(hls.iter().all(|a| a.start < a.end), "highlights carry a real char range");
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
        let start = Point::new((r0.x0 + r0.x1) / 2.0, (r0.y0 + r0.y1) / 2.0);
        let later = app.sel_runs.iter().find(|r| r.block > r0.block).cloned().unwrap();
        let end = Point::new((later.x0 + later.x1) / 2.0, (later.y0 + later.y1) / 2.0);
        ptr_move(&mut app, &mut doc, start);
        ptr_down(&mut app, &mut doc);
        ptr_move(&mut app, &mut doc, end);
        ptr_up(&mut app, &mut doc);
        assert!(app.sel.map_or(false, |s| !s.is_empty()));

        // The toolbar's Note action opens the composer (not the side rail),
        // anchored to the selection, with the selection's text as the excerpt.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let note = find_hit(&app, |a| matches!(a, Action::AddNote)).expect("a Note action");
        assert!(click(&mut app, &mut doc, note), "note handled");
        let cid = app.composing.expect("a note is being composed on the page");
        let a = BooksDoc::list_annotations(&doc).into_iter().find(|a| a.id == cid).unwrap();
        assert_eq!(a.kind, AnnoKind::Note);
        assert!(!a.excerpt.is_empty(), "the composer note carries the selected passage");
        // The on-page composer records a Save hit (proving it's drawn on-page).
        frame(&mut app, &mut doc, 1000.0, 700.0);
        assert!(find_hit(&app, |ac| matches!(ac, Action::FinishNote)).is_some(), "on-page composer is drawn");
    }

    #[test]
    fn settings_popover_does_not_dim_the_page() {
        use ui::offscreen::OffscreenRenderer;
        let (w, h) = (1000u32, 700u32);
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Same page, popover closed then open.
        let closed = render_scene(&mut app, &mut doc, w as f64, h as f64);
        app.type_popover = true;
        let open = render_scene(&mut app, &mut doc, w as f64, h as f64);

        let mut ro = OffscreenRenderer::new(w, h);
        let c_px = ro.render_rgba(&closed);
        let o_px = ro.render_rgba(&open);
        // A point well inside the page column, above/left of the bottom-right
        // popover panel — if a scrim dimmed the page it would darken here.
        let (x, y) = (400u32, 250u32);
        let a0 = OffscreenRenderer::sample(&c_px, w, x, y);
        let a1 = OffscreenRenderer::sample(&o_px, w, x, y);
        let d = |u: u8, v: u8| (u as i32 - v as i32).abs();
        assert!(
            d(a0.0, a1.0) <= 3 && d(a0.1, a1.1) <= 3 && d(a0.2, a1.2) <= 3,
            "the open settings popover must not dim the page: {a0:?} vs {a1:?}",
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
        assert!(app.shelf_scroll <= app.shelf_max_scroll + 0.001, "never past the end");
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
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let (c0, p0) = (app.chapter, app.page);
        let next = find_hit(&app, |a| matches!(a, Action::NextPage)).expect("next button");
        assert!(click(&mut app, &mut doc, next), "next handled");
        assert!(
            app.page != p0 || app.chapter != c0,
            "position advanced from ch{c0}/pg{p0} to ch{}/pg{}",
            app.chapter,
            app.page
        );
    }

    #[test]
    fn selecting_a_paragraph_then_a_swatch_writes_a_highlight() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let before = BooksDoc::list_annotations(&doc).len();
        // Select a paragraph via its page hit.
        let block = find_hit(&app, |a| matches!(a, Action::SelectBlock(_))).expect("a block");
        assert!(click(&mut app, &mut doc, block), "select handled");
        assert!(app.selected_block.is_some());
        // The selection HUD now offers colour swatches — click one to highlight.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        assert!(click(&mut app, &mut doc, swatch), "swatch handled");
        let after = BooksDoc::list_annotations(&doc);
        assert_eq!(after.len(), before + 1, "one highlight added");
        assert!(after.iter().any(|a| a.kind == AnnoKind::Highlight));
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
        assert!(BooksDoc::list_annotations(&doc).is_empty(), "no seeded annotations");
    }

    // ---- library-shelf keyboard (was inert: handle_key returned false) ------

    #[test]
    fn library_keyboard_moves_focus_and_opens_the_focused_book() {
        let mut app = app_with_sample();
        app.view = View::Library;
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 720.0);
        assert!(app.library.len() >= 3, "sample shelf has several books");
        assert_eq!(app.shelf_cursor, 0, "focus starts on the first book");

        // Arrow-right/-left move the focus along the row.
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowRight), "arrow consumed");
        assert_eq!(app.shelf_cursor, 1, "arrow-right moved focus");
        assert!(key_named(&mut app, &mut doc, NamedKey::ArrowLeft));
        assert_eq!(app.shelf_cursor, 0, "arrow-left moved focus back");

        // End jumps to the last book; Home back to the first.
        assert!(key_named(&mut app, &mut doc, NamedKey::End));
        assert_eq!(app.shelf_cursor, app.library.len() - 1, "End -> last book");
        assert!(key_named(&mut app, &mut doc, NamedKey::Home));
        assert_eq!(app.shelf_cursor, 0, "Home -> first book");

        // Enter opens the focused book into the reader.
        assert!(key_named(&mut app, &mut doc, NamedKey::Enter), "enter consumed");
        assert_eq!(app.view, View::Reader, "enter opened the focused book");
        assert_eq!(app.book_idx, 0, "opened the focused (first) book");
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
        assert_eq!(app.shelf_cursor, 1, "down stepped one row (one book) in a single column");
    }

    // ---- empty-shelf reload (was a static dead end) -------------------------

    #[test]
    #[serial]
    fn empty_shelf_reload_button_repopulates_via_the_real_load_path() {
        // Point discovery at an empty directory: load_library() then yields only
        // the bundled starter, so the reload has an observable effect.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("EBOOK_LIBRARY_DIR", tmp.path()) };

        let mut app = BooksApp::with_library(Vec::new());
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 900.0, 640.0);
        assert!(app.library.is_empty(), "starts on a genuinely empty shelf");
        assert_eq!(app.view, View::Library);

        let reload = find_hit(&app, |a| matches!(a, Action::ReloadLibrary))
            .expect("empty shelf offers a Reload button");
        assert!(click(&mut app, &mut doc, reload), "reload click handled");
        assert!(!app.library.is_empty(), "reload discovered books through the real load path");

        unsafe { std::env::remove_var("EBOOK_LIBRARY_DIR") };
    }

    // ---- delete affordance (was unreachable without hover) ------------------

    #[test]
    fn annotation_delete_is_reachable_and_removes_it() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        // Wide window so the annotations rail is docked-open.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        // Create one highlight via select + swatch.
        let block = find_hit(&app, |a| matches!(a, Action::SelectBlock(_))).expect("a block");
        click(&mut app, &mut doc, block);
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let swatch = find_hit(&app, |a| matches!(a, Action::SetColor(_))).expect("a swatch");
        click(&mut app, &mut doc, swatch);
        assert_eq!(BooksDoc::list_annotations(&doc).len(), 1, "one highlight created");

        // The delete (×) is registered without the pointer hovering the card
        // (the cursor is still over the swatch on the page), proving reachability.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let del = find_hit(&app, |a| matches!(a, Action::Delete(_)))
            .expect("delete affordance is present without hover");
        assert!(click(&mut app, &mut doc, del), "delete click handled");
        assert!(BooksDoc::list_annotations(&doc).is_empty(), "annotation removed");
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
        // Select a paragraph, then start a note with the 'm' shortcut.
        let block = find_hit(&app, |a| matches!(a, Action::SelectBlock(_))).expect("a block");
        click(&mut app, &mut doc, block);
        assert!(key_char(&mut app, &mut doc, "m"), "'m' started a note");
        let nid = app.composing.expect("a note is now being composed");

        for ch in ["H", "i"] {
            assert!(key_char(&mut app, &mut doc, ch), "char consumed while composing");
        }
        assert!(key_named(&mut app, &mut doc, NamedKey::Space), "space consumed");
        for ch in ["t", "h", "e", "r", "e"] {
            assert!(key_char(&mut app, &mut doc, ch));
        }
        assert!(key_named(&mut app, &mut doc, NamedKey::Backspace), "backspace consumed");
        assert!(key_named(&mut app, &mut doc, NamedKey::Enter), "enter finished the note");
        assert!(app.composing.is_none(), "enter ended composing");

        let note = BooksDoc::list_annotations(&doc)
            .into_iter()
            .find(|a| a.id == nid)
            .expect("the composed note is in the document");
        assert_eq!(note.kind, AnnoKind::Note);
        assert_eq!(note.note, "Hi ther", "typed text with one backspace persisted");
    }

    #[test]
    fn clicking_a_finished_note_reopens_it_for_editing() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        frame(&mut app, &mut doc, 1280.0, 800.0);
        let block = find_hit(&app, |a| matches!(a, Action::SelectBlock(_))).expect("a block");
        click(&mut app, &mut doc, block);
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
        app.selected_block = None;
        let jump = find_hit(&app, |ac| matches!(ac, Action::JumpTo(_))).expect("annotation card");
        assert!(click(&mut app, &mut doc, jump), "jump handled");
        assert_eq!(app.chapter, 0, "jumped to the annotation's chapter");
        assert_eq!(app.selected_block, Some(target), "jumped to the annotation's block");
        // The deferred page resolution runs on the next settled frame.
        frame(&mut app, &mut doc, 1280.0, 800.0);
        assert!(app.pending_jump_block.is_none(), "pending jump was consumed by layout");
    }

    #[test]
    fn page_for_block_maps_a_block_to_its_page() {
        use crate::model::{Block, Span};
        use crate::paginate::layout_chapter;
        let mut tr = TextRenderer::new();
        // Eight fat paragraphs into a small box -> several pages.
        let blocks: Vec<Block> = (0..8)
            .map(|_| Block::Paragraph { spans: vec![Span::plain("word ".repeat(60))] })
            .collect();
        let layout = layout_chapter(
            &mut tr,
            &blocks,
            300.0,
            160.0,
            &Settings::default(),
            0,
            &FontChoice::SansSerif,
            1.0,
            0,
        );
        assert!(layout.page_count() >= 2, "content spans multiple pages");
        assert_eq!(page_for_block(&layout, 0), 0, "first block is on page 0");
        let last = layout.blocks.last().unwrap().index;
        assert!(page_for_block(&layout, last) >= 1, "the last block is past page 0");
        for b in &layout.blocks {
            assert!(page_for_block(&layout, b.index) < layout.page_count(), "valid page index");
        }
        assert_eq!(page_for_block(&layout, 9999), 0, "unknown block -> safe page 0");
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
        let row = find_hit(&app, |a| matches!(a, Action::GotoChapter(c) if *c == target))
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
        let inc = find_hit(&app, |a| matches!(a, Action::FontInc)).expect("font + stepper");
        click(&mut app, &mut doc, inc);
        let grown = BooksDoc::read_settings(&doc).font_scale;
        assert!(grown > base, "font scale grew and persisted ({base} -> {grown})");

        // Line-spacing − button shrinks and persists the spacing.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let sbase = BooksDoc::read_settings(&doc).line_spacing;
        let dec = find_hit(&app, |a| matches!(a, Action::SpacingDec)).expect("spacing - stepper");
        click(&mut app, &mut doc, dec);
        let shrunk = BooksDoc::read_settings(&doc).line_spacing;
        assert!(shrunk < sbase, "line spacing shrank and persisted ({sbase} -> {shrunk})");
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
        assert_ne!((app.chapter, app.page), start, "arrow-right turned the page");

        // 't' toggles the docked contents rail.
        frame(&mut app, &mut doc, 1000.0, 700.0);
        let toc0 = app.toc_open();
        assert!(key_char(&mut app, &mut doc, "t"), "'t' consumed");
        assert_ne!(app.toc_open(), toc0, "'t' toggled the contents rail");

        // Digit keys set the page tint and persist it (Light differs from the
        // Sepia default, so this proves a real write, not a coincidence).
        assert!(key_char(&mut app, &mut doc, "1"));
        assert_eq!(BooksDoc::read_settings(&doc).theme, ReadTheme::Light, "'1' set Light");
        assert!(key_char(&mut app, &mut doc, "3"));
        assert_eq!(BooksDoc::read_settings(&doc).theme, ReadTheme::Dark, "'3' set Dark");

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
        assert_eq!(reopened.chapter, reached, "reopen restored the saved chapter");
    }
}
