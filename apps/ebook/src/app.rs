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

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};
use ui::ColorCompat;

use crate::content::load_library;
use crate::doc::BooksDoc;
use crate::model::{
    now_ms, AnnoKind, Annotation, Book, HiColor, Progress, ReadFont, ReadTheme, Settings,
};
use crate::paginate::{layout_chapter, ChapterLayout, LayoutKey, LayoutKind};
use crate::paint;
use crate::reader_theme::ReaderPalette;

const TOP_BAR: f64 = 54.0;
const BOTTOM_BAR: f64 = 58.0;

/// Pixels one wheel-line notch maps to, matching the toolkit's other scroll
/// surfaces so a mouse wheel feels consistent across apps.
const WHEEL_LINE_PX: f64 = 48.0;
/// Accumulated reader wheel travel (px) that commits to one page turn.
const TURN_STEP_PX: f64 = 40.0;
const TOC_W: f64 = 244.0;
const NOTES_W: f64 = 326.0;
const PAGE_MAX_W: f64 = 680.0;

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
}

/// A clickable region recorded during `draw`, consumed on the next click.
#[derive(Clone)]
struct Hit {
    rect: Rect,
    action: Action,
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
    SetReadTheme(ReadTheme),
    SetColor(HiColor),
    SelectBlock(usize),
    AddNote,
    JumpTo(Uuid),
    Delete(Uuid),
    EditNote(Uuid),
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
    hi_color: HiColor,
    /// Vertical scroll offset of the library shelf (logical px). The shelf can
    /// hold far more books than fit on screen, so it scrolls; this is the
    /// current offset, clamped each frame to `shelf_max_scroll`.
    shelf_scroll: f64,
    /// Last-known maximum shelf scroll, computed during `draw_library` where the
    /// column count + row heights are known, and read back by the wheel handler
    /// (which runs before the next layout) to clamp the offset.
    shelf_max_scroll: f64,
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

    // --- caches pulled from the CRDT (refreshed in sync / after events) ----
    settings: Settings,
    annotations: Vec<Annotation>,
    shelf_fraction: HashMap<String, f32>,
    shelf_annos: HashMap<String, usize>,
    last_persisted: Option<(usize, usize)>,
    cur_page_count: usize,
    seeded: bool,
    /// Sample-content mode: only the hermetic `--shots`/test path sets this. It
    /// swaps in the multi-book sample library and seeds demo annotations so a
    /// screenshot shows a lived-in reader. A real user is never seeded.
    demo: bool,
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
            hi_color: HiColor::Yellow,
            shelf_scroll: 0.0,
            shelf_max_scroll: 0.0,
            turn_accum: 0.0,
            type_popover: false,
            cursor: Point::ZERO,
            page_top_block: 0,
            composing: None,
            pending_last_page: false,
            pending_jump_block: None,
            settings: Settings::default(),
            annotations: Vec::new(),
            shelf_fraction: HashMap::new(),
            shelf_annos: HashMap::new(),
            last_persisted: None,
            cur_page_count: 1,
            seeded: false,
            demo: false,
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
            "select" => {
                // A selected paragraph so the shot exercises the floating
                // selection toolbar (swatches + Note) over the page.
                self.view = View::Reader;
                self.selected_block = Some(2);
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
            Action::CycleFont => self.bump_setting(doc, |s| {
                s.font = match s.font {
                    ReadFont::Sans => ReadFont::Mono,
                    ReadFont::Mono => ReadFont::Sans,
                }
            }),
            Action::SetReadTheme(t) => self.bump_setting(doc, |s| s.theme = t),
            Action::SetColor(c) => {
                self.hi_color = c;
                // Selecting a colour with a block already selected is the
                // fastest path to a highlight: do it in one click.
                if let Some(b) = self.selected_block {
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
                if let Some(b) = self.selected_block {
                    let id = self.book_id();
                    let excerpt = self.block_excerpt(b);
                    let mut a = Annotation::new(&id, AnnoKind::Note, self.chapter, b)
                        .with_color(self.hi_color)
                        .with_excerpt(excerpt);
                    a.note = String::new();
                    let aid = a.id;
                    BooksDoc::upsert_annotation(doc, &a);
                    self.composing = Some(aid);
                    self.notes_flap.open();
                }
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
        }
    }

    fn book_id(&self) -> String {
        self.library
            .get(self.book_idx)
            .map(|b| b.id.clone())
            .unwrap_or_default()
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

// ==========================================================================
// Widget impl
// ==========================================================================

impl Widget for BooksApp {
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        self.seed_if_needed(doc);
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
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Topmost wins: later-recorded hits draw on top.
                if let Some(h) = self
                    .hits
                    .iter()
                    .rev()
                    .find(|h| h.rect.contains(self.cursor))
                    .cloned()
                {
                    self.dispatch(h.action, doc);
                    self.refresh_from_doc(doc);
                    self.persist_progress(doc);
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
                // summed so a flick is a controlled turn, not a stampede.
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-*y as f64) * WHEEL_LINE_PX,
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                if self.handle_wheel(dy, doc) {
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
    /// Keyboard handling. Returns `true` if the key was consumed (so the App's
    /// focus-navigation fallback doesn't also steal the arrows).
    fn handle_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
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
            // Enter / space opens the first book; arrows could move a cursor —
            // keep it simple and let clicks drive the shelf.
            return false;
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

    /// Wheel / trackpad handling. In the reader a downward scroll turns forward
    /// and an upward scroll turns back, accumulated so a trackpad flick advances
    /// a sensible number of pages. On the shelf it scrolls the grid so a library
    /// taller than the window is fully reachable. Returns `true` when consumed.
    fn handle_wheel(&mut self, dy: f64, doc: &mut AutoCommit) -> bool {
        if dy == 0.0 {
            return false;
        }
        match self.view {
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
            let hint = "Add .epub files to your Books folder to see them here.";
            let (tw2, _) = tr.measure(hint, 13.0, None);
            tr.draw_text(
                scene,
                hint,
                13.0,
                c.text_dim,
                t * Affine::translate((cx - tw2 as f64 / 2.0, cy + 4.0)),
                None,
            );
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
            self.draw_book_card(scene, t, tr, theme, book, i, x, y, card_w, cover_h);
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
        if hover {
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

        let palette = ReaderPalette::resolve(self.settings.theme, theme);

        // ---- paper rect + content box -----------------------------------
        let center_w = (center_x1 - center_x0).max(120.0);
        let paper_w = (center_w - 56.0).min(PAGE_MAX_W).max(120.0);
        let paper_x = center_x0 + (center_w - paper_w) / 2.0;
        let paper_y = body_y0 + 18.0;
        let paper_h = (body_y1 - body_y0 - 36.0).max(120.0);
        let pad_x = 46.0;
        let pad_top = 50.0;
        let pad_bot = 46.0;
        let content_x = paper_x + pad_x;
        let content_y = paper_y + pad_top;
        let content_w = (paper_w - 2.0 * pad_x).max(60.0) as f32;
        let content_h = (paper_h - pad_top - pad_bot).max(60.0) as f32;

        // ---- (re)compute the chapter layout -----------------------------
        let key = LayoutKey::new(self.chapter, content_w, content_h, &self.settings);
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
        let shadow = Rect::new(paper_x - 2.0, paper_y + 4.0, paper_x + paper_w + 2.0, paper_y + paper_h + 8.0);
        paint::fill_round(scene, t, shadow, 10.0, Color::from_rgba8(0, 0, 0, 64));
        paint::fill_round(scene, t, paper, 8.0, palette.page);
        paint::stroke_round(scene, t, paper, 8.0, palette.page_edge, 1.0);

        // running header (chapter title)
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

            // selected-block outline
            if self.selected_block == Some(b.index) {
                let sel = Rect::new(
                    content_x + b.indent as f64 - 8.0,
                    by0 - 2.0,
                    content_x + content_w as f64 + 6.0,
                    by1 + 2.0,
                );
                paint::fill_round(scene, t, sel, 6.0, theme.accent_primary.with_alpha_factor(0.10));
                paint::stroke_round(scene, t, sel, 6.0, theme.accent_primary.with_alpha_factor(0.55), 1.2);
                sel_anchor = Some(by0);
            }

            for a in annos.iter().filter(|a| a.chapter == chapter && a.block == b.index) {
                match a.kind {
                    AnnoKind::Highlight => {
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

        // ---- text + decorations (on top of bands) -----------------------
        let base_mono = self.settings.font.is_mono();
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
                    // inline-code chip
                    if run.mono && !base_mono {
                        let (cw, _) = tr.measure_styled(&run.text, b.size, None, weight, run.italic, true);
                        let chip = Rect::new(x - 3.0, ly - 1.0, x + cw as f64 + 3.0, ly + b.size as f64 + 3.0);
                        paint::fill_round(scene, t, chip, 3.0, palette.code_bg);
                    }
                    tr.draw_text_styled(
                        scene,
                        &run.text,
                        b.size,
                        palette.ink,
                        t * Affine::translate((x, ly)),
                        None,
                        weight,
                        run.italic,
                        mono,
                    );
                }
            }
        }

        // ---- page footer -------------------------------------------------
        let footer = format!(
            "Page {} of {}   ·   Chapter {} of {}",
            self.page + 1,
            layout.page_count(),
            self.chapter + 1,
            nchap.max(1)
        );
        let (fw, _) = tr.measure(&footer, 11.0, None);
        tr.draw_text(
            scene,
            &footer,
            11.0,
            palette.ink_dim,
            t * Affine::translate((paper_x + (paper_w - fw as f64) / 2.0, paper_y + paper_h - 30.0)),
            None,
        );
        // thin book-progress bar at the very bottom of the paper
        let frac = if nchap > 0 {
            (self.chapter as f32 + (self.page as f32 + 1.0) / layout.page_count() as f32) / nchap as f32
        } else {
            0.0
        };
        let pbar = Rect::new(content_x, paper_y + paper_h - 16.0, content_x + content_w as f64, paper_y + paper_h - 13.0);
        paint::progress(scene, t, pbar, frac, palette.rule, theme.accent_primary);

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
        if self.view != View::Reader || self.selected_block.is_none() {
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
            let right_bound = r.x1 - 14.0 - s - 6.0 - s - 12.0;
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
        rx -= s + 6.0;
        self.icon_button(
            scene,
            t,
            theme,
            rx,
            cy,
            s,
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
        // Tap-away scrim (very light — this is a small utility popover).
        let scrim = Color::from_rgba8(0, 0, 0, 255).with_alpha_factor(0.18);
        scene.fill(Fill::NonZero, t, scrim, None, &r);
        self.push_hit(r, Action::DismissOverlay);

        let pad = 16.0;
        let row_h = 34.0;
        let panel_w = 300.0_f64.min(r.width() - 24.0);
        let panel_h = pad * 2.0 + row_h * 4.0 + 10.0 * 3.0 + 22.0 * 2.0;
        let bar_top = r.y1 - BOTTOM_BAR;
        let px1 = (r.x1 - 18.0).min(r.x1 - 12.0);
        let px0 = (px1 - panel_w).max(r.x0 + 12.0);
        let py1 = bar_top - 10.0;
        let py0 = py1 - panel_h;
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

        // Typeface segmented.
        tr.draw_text_weighted(scene, "TYPEFACE", 10.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 16.0;
        let mut sx = ix0;
        for f in ReadFont::ALL {
            let active = self.settings.font == f;
            let action = if active { Action::Swallow } else { Action::CycleFont };
            sx += self.pill(scene, t, tr, theme, sx, y, row_h - 4.0, f.label(), 12.5, active, Some(action)) + 8.0;
        }
        y += (row_h - 4.0) + 22.0;

        // Page tint segmented.
        tr.draw_text_weighted(scene, "PAGE", 10.0, c.text_dim, t * Affine::translate((ix0, y)), None, TextWeight::Semibold);
        y += 16.0;
        let mut sx = ix0;
        for rt in ReadTheme::ALL {
            let active = self.settings.theme == rt;
            sx += self.pill(scene, t, tr, theme, sx, y, row_h - 4.0, rt.label(), 12.5, active, Some(Action::SetReadTheme(rt))) + 8.0;
        }
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
            // delete affordance
            let del = Rect::new(card_x1 - 26.0, y + 6.0, card_x1 - 6.0, y + 26.0);
            if hover {
                paint::close(scene, t, del, c.text_dim, 1.4);
                self.push_hit(del, Action::Delete(a.id));
            }

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
    use ui::presence::PresenceCtx;
    use winit::dpi::PhysicalPosition;
    use winit::event::DeviceId;

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
}
