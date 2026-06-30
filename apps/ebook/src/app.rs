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
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
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
#[derive(Clone, Copy, PartialEq, Eq)]
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

    // --- layout cache ------------------------------------------------------
    layout: Option<ChapterLayout>,

    // --- per-frame hit regions --------------------------------------------
    hits: Vec<Hit>,
}

impl BooksApp {
    pub fn new() -> Self {
        let library = load_library();
        Self {
            library,
            rect: Rect::ZERO,
            dirty: true,
            // Default to the reader on the bundled book: the money shot is a
            // real reading page with chrome, and the library is one click away.
            view: View::Reader,
            book_idx: 0,
            chapter: 0,
            page: 0,
            toc_flap: FlapState::open_when_docked(),
            notes_flap: FlapState::open_when_docked(),
            selected_block: Some(2),
            hi_color: HiColor::Yellow,
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
            layout: None,
            hits: Vec::new(),
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

    /// Reconcile both flaps with the current width (drop a stale user
    /// preference when the window crosses a breakpoint) and advance their
    /// eased reveal. Returns true while either is still animating. Called once
    /// per frame from `draw` so a resize returns to the auto behaviour.
    fn reconcile_flaps(&mut self) {
        let (tl, nl) = (self.toc_layout(), self.notes_layout());
        self.toc_flap.reconcile(tl);
        self.notes_flap.reconcile(nl);
    }

    /// True while either rail's eased reveal is still settling toward its
    /// target — drives the host's "request another frame" decision.
    fn flaps_animating(&self) -> bool {
        self.toc_flap.is_animating(self.toc_layout())
            || self.notes_flap.is_animating(self.notes_layout())
    }

    /// Stage a named interaction state for the single-frame `--shots`
    /// renderer (which can't reach these states through live input). The
    /// flaps are *snapped* (no animation) so the captured frame is settled.
    ///
    /// Recognised scenarios:
    /// * `narrow` — summon the contents rail as an overlay flap over the page
    ///   behind a scrim, the state a narrow window shows after the hamburger.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        if scenario == "narrow" {
            // Open the contents rail; the annotations rail stays closed so the
            // scrimmed overlay flap reads clearly against the page.
            self.toc_flap.snap(true);
            self.notes_flap.snap(false);
        }
    }

    // ======================================================================
    // CRDT <-> cache plumbing
    // ======================================================================

    /// Seed sensible first-run content so a brand-new document opens onto a
    /// living page (a bookmark, two highlights, and a typed note) instead of a
    /// blank one. Guarded so a returning reader is never re-seeded.
    fn seed_if_needed(&mut self, doc: &mut AutoCommit) {
        if self.seeded {
            return;
        }
        self.seeded = true;

        if !BooksDoc::has_settings(doc) {
            BooksDoc::write_settings(doc, &Settings::default());
        }
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
            Action::DismissOverlay => {
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
                // Esc first dismisses an open overlay flap (narrow window);
                // only when none is floating does it leave the book.
                let toc_overlay = self.toc_layout() == SplitLayout::Overlay && self.toc_open();
                let notes_overlay =
                    self.notes_layout() == SplitLayout::Overlay && self.notes_open();
                if toc_overlay || notes_overlay {
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
        let total_annos: usize = self.shelf_annos.values().sum();
        let sub = format!("{} books · {} annotations", lib.len(), total_annos);
        let (sw, _) = tr.measure(&sub, 13.0, None);
        tr.draw_text(
            scene,
            &sub,
            13.0,
            c.text_dim,
            t * Affine::translate((r.x1 - 28.0 - sw as f64, r.y0 + 19.0)),
            None,
        );

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

        for (i, book) in lib.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let x = start_x + col as f64 * (card_w + gap);
            let y = start_y + row as f64 * (card_h + gap);
            self.draw_book_card(scene, t, tr, theme, book, i, x, y, card_w, cover_h);
        }

        self.library = lib;
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

        // Drop shadow + cover.
        let shadow = Rect::new(x + 3.0, y + 6.0, x + w + 3.0, y + cover_h + 6.0);
        paint::fill_round(scene, t, shadow, 12.0, Color::from_rgba8(0, 0, 0, 60));
        let (spine, ink) = cover_colors(theme, book.cover_tint);
        paint::fill_round(scene, t, cover, 12.0, spine);
        // a darker foot for depth
        let foot = Rect::new(x, y + cover_h * 0.72, x + w, y + cover_h);
        paint::fill_round(scene, t, foot, 12.0, Color::from_rgba8(0, 0, 0, 38));
        if hover {
            paint::stroke_round(scene, t, cover, 12.0, theme.border_focus, 2.0);
        }

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

        self.push_hit(
            Rect::new(x, y, x + w, meta_y + 70.0),
            Action::OpenBook(idx),
        );
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

        // ---- chrome: top bar, bottom bar, drawers -----------------------
        self.draw_reader_topbar(scene, t, tr, theme, &lib, &c);
        self.draw_reader_bottombar(scene, t, tr, theme, &c);
        self.draw_rails(scene, t, tr, theme, &lib, body_y0, body_y1, &c);

        self.library = lib;
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

        // centre: title + author
        if let Some(book) = lib.get(self.book_idx) {
            let title = &book.title;
            let (tw, _) = tr.measure_weighted(title, 16.0, None, TextWeight::Semibold);
            let auth = format!("  ·  {}", book.author);
            let (aw, _) = tr.measure(&auth, 13.0, None);
            let total = tw as f64 + aw as f64;
            let cx = (r.x0 + r.x1) / 2.0 - total / 2.0;
            tr.draw_text_weighted(
                scene,
                title,
                16.0,
                c.text_primary,
                t * Affine::translate((cx, r.y0 + 17.0)),
                None,
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &auth,
                13.0,
                c.text_dim,
                t * Affine::translate((cx + tw as f64, r.y0 + 18.5)),
                None,
            );
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

    #[allow(clippy::too_many_arguments)]
    fn draw_reader_bottombar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        c: &ui::tokens::colors::Colors,
    ) {
        let r = self.rect;
        let y0 = r.y1 - BOTTOM_BAR;
        let bar = Rect::new(r.x0, y0, r.x1, r.y1);
        paint::fill_rect(scene, t, bar, c.surfaces.raised);
        paint::line(scene, t, Point::new(r.x0, y0), Point::new(r.x1, y0), c.border_standard, 1.0);

        let s = 34.0;
        let cy = y0 + (BOTTOM_BAR - s) / 2.0;
        let pill_h = 30.0;
        let pill_y = y0 + (BOTTOM_BAR - pill_h) / 2.0;
        let mut x = r.x0 + 18.0;

        // page nav
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
        x += pw as f64 + 14.0;
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::Next, false, Action::NextPage) + 22.0;

        // typography group
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::MinusI, false, Action::FontDec) + 2.0;
        let fs = format!("{}%", (self.settings.font_scale * 100.0).round() as i32);
        let (fsw, _) = tr.measure(&fs, 12.0, None);
        tr.draw_text(scene, &fs, 12.0, c.text_dim, t * Affine::translate((x + 4.0, cy + 9.0)), None);
        x += fsw as f64 + 8.0;
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::PlusI, false, Action::FontInc) + 12.0;
        x += self.pill(scene, t, tr, theme, x, pill_y, pill_h, self.settings.font.label(), 12.0, false, Some(Action::CycleFont)) + 8.0;

        // line spacing
        let ls = format!("Line {:.1}", self.settings.line_spacing);
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::MinusI, false, Action::SpacingDec) + 2.0;
        let (lsw, _) = tr.measure(&ls, 12.0, None);
        tr.draw_text(scene, &ls, 12.0, c.text_dim, t * Affine::translate((x + 4.0, cy + 9.0)), None);
        x += lsw as f64 + 8.0;
        x += self.icon_button(scene, t, theme, x, cy, s, Ico::PlusI, false, Action::SpacingInc) + 22.0;

        // appearance: reading-theme segmented (right-aligned)
        let mut groups_x = x;
        for rt in ReadTheme::ALL {
            let active = self.settings.theme == rt;
            groups_x += self.pill(
                scene,
                t,
                tr,
                theme,
                groups_x,
                pill_y,
                pill_h,
                rt.label(),
                12.0,
                active,
                Some(Action::SetReadTheme(rt)),
            ) + 6.0;
        }
        groups_x += 14.0;

        // highlight colour swatches
        for col in HiColor::ALL {
            let r9 = 9.0;
            let center = Point::new(groups_x + r9, pill_y + pill_h / 2.0);
            let rect = Rect::new(center.x - r9, center.y - r9, center.x + r9, center.y + r9);
            paint::circle(scene, t, center, r9, ReaderPalette::highlight_swatch(col));
            if self.hi_color == col {
                paint::stroke_round(scene, t, rect.inflate(2.0, 2.0), r9 + 2.0, theme.text_primary, 1.6);
            } else if self.is_hover(rect) {
                paint::stroke_round(scene, t, rect.inflate(2.0, 2.0), r9 + 2.0, theme.border_standard, 1.2);
            }
            self.push_hit(rect.inflate(3.0, 3.0), Action::SetColor(col));
            groups_x += r9 * 2.0 + 8.0;
        }
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
