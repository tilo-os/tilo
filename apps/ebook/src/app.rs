//! The Books app widget — a single retained-mode [`Widget`] that renders both
//! the library shelf and the paginated reader, sourcing colour from the OS
//! [`Theme`] (chrome) or a reader palette (the page), and storing all user
//! state in the Automerge CRDT.
//!
//! One custom widget rather than composed `ui` widgets because reflowable,
//! paginated text with highlight bands behind glyphs and margin annotation
//! glyphs doesn't map onto stock layout widgets: the surface is drawn
//! straight into the Vello [`Scene`] via [`crate::paginate`] and
//! [`crate::paint`]. `draw` records clickable `Hit` regions; `handle_event`
//! hit-tests them on the next click; a frame always precedes input so
//! regions are never stale.
//!
//! State: the library record ([`Ebook`]) holds the
//! shelf, settings, progress and annotations, changed through its field
//! edits and verbs. Ephemeral widget caches (open book, page index,
//! selection, layout cache) are per-peer view state kept out of the document
//! so readers browse independently.

#![allow(
    clippy::many_single_char_names,
    reason = "t (Affine), c (Colors), r (Rect), x/y/w/h (geometry) and their close relatives \
              are the standard vocabulary shared by every draw/hit-test helper in this file"
)]

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use tilo_clock::unix_now_ms;
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Blob, Color, Fill, ImageData as PenikoImage};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::time::Instant;

use ui::ColorCompat;
use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::chrome::NavRowFace;
use ui::chrome::SelectionState;
use ui::gesture::{DragSelect, Granularity};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::portal::{FileFilter, PickRequest};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::metrics::Metrics;
use ui::widget::modal_dialog;
use ui::widget::toast::{ToastKind, Transient};
use ui::widget::{
    Button, ButtonFace, ButtonRole, CardFace, ChipFace, ChipLead, ColorSwatchFace, Continuable,
    ControlSize, DropZoneFace, EditOutcome, EditableLabel, EmptyState, EmptyStateContent,
    InputHint, Interaction, ListRowFace, MediaArt, MediaAspect, MediaSize, ModalAction, ModalSpec,
    ScrollMetrics, Scrollbar, SegmentedFace, SegmentedStyle, SidebarDrawer, Slider, SliderFace,
    Toggle, ToggleFace, ToggleGroup, Widget, card_column_width, card_columns,
};
use ui::window_toolbar::ToolbarNotice;

use crate::comment_overlay::{CommentAction, CommentOverlay};
use crate::content::{self, is_shared};
use crate::hosted_dropdown::HostedDropdown;
use crate::input_controls::{
    ColorEdit, ColorSheetResponse, InputEventCtx, NoteAction, NoteComposer, NoteSemanticOutcome,
    NumberControls, NumberField, PickTarget, SchemeColorSheet,
};
use crate::local_library::LocalLibrary;
use crate::model::{
    AnnoKind, Annotation, Book, ColorScheme, CompositionStyle, FontEntry, FontKind, HiColor,
    ReadMode, ReadTheme, Settings, default_fonts, default_schemes,
};
use crate::paginate::{ChapterLayout, LayoutKey, LayoutKind, font_choice};
use crate::paint;
use crate::reader_bubble::{ReaderBubble, ReaderBubbleAction, ReaderPosition};
use crate::reader_theme::ReaderPalette;
use crate::reading_settings::{Appearance, ReadingSettings, ReadingSettingsAction};
use crate::resource::{Ebook, HiPiece, Passage, books};
use crate::shelf_controls::{ShelfControlAction, ShelfControls};
use ui::lock::MutexRecover as _;

/// Whether a parsed book should replace the reader's current content or only
/// join a watched shelf. The worker carries this presentation intent but never
/// touches the document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImportKind {
    Open,
    Watch,
}

struct ImportRequest {
    generation: u64,
    path: PathBuf,
    kind: ImportKind,
    expected_book_id: Option<String>,
    /// Where to fetch the book from into `path` first: a book got from the
    /// store.
    fetch: Option<String>,
    /// A test's hold on the worker: it reports on the first channel that it
    /// took this request, then parses only once the second one fires or is
    /// dropped.
    #[cfg(test)]
    hold: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

struct ImportResult {
    generation: u64,
    /// The user-granted or watched source. This is used exclusively to decide
    /// whether a worker result is still current and to de-duplicate watches;
    /// it is never written to the collaborative document.
    source_path: PathBuf,
    /// The resolved shelf location: normally the content-addressed workspace
    /// copy, otherwise the source when the host has no workspace capability.
    path: PathBuf,
    kind: ImportKind,
    expected_book_id: Option<String>,
    result: Result<Book, String>,
}

/// A bounded, latest-wins worker mailbox. At most one EPUB is being parsed and
/// one newer request is retained; a burst of watched files cannot create an
/// unbounded queue or block an event handler.
struct ImportMailbox {
    pending: Option<ImportRequest>,
    closed: bool,
}

struct ImportWorker {
    mailbox: Arc<(Mutex<ImportMailbox>, Condvar)>,
    results: mpsc::Receiver<ImportResult>,
}

impl ImportWorker {
    fn new() -> Result<Self, String> {
        let mailbox = Arc::new((
            Mutex::new(ImportMailbox {
                pending: None,
                closed: false,
            }),
            Condvar::new(),
        ));
        let (result_tx, results) = mpsc::sync_channel(1);
        let worker_mailbox = Arc::clone(&mailbox);
        let ui_wake = ui::wake::WakeHandle::new();
        ui::rt::spawn_thread("ebook-import-worker", move || {
            loop {
                let request = {
                    let (lock, wake) = &*worker_mailbox;
                    let mut state = lock.lock_recover();
                    while state.pending.is_none() && !state.closed {
                        state = wake
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    if state.closed {
                        return;
                    }
                    state.pending.take()
                };
                let Some(request) = request else { continue };
                #[cfg(test)]
                if let Some((entered, release)) = &request.hold {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                // The file is copied into the content-addressed workspace
                // on this worker before its collaborative library row is
                // written on the UI thread.  A peer therefore receives
                // bytes and metadata as one logical import, never a
                // dangling shared title/annotation record.
                let fetched = match &request.fetch {
                    Some(url) => crate::store::download(url, &request.path),
                    None => Ok(()),
                };
                let parsed =
                    fetched.and_then(|()| content::parse_shared_path_detailed(&request.path));
                let (path, result) = match parsed {
                    Ok((book, shared_path))
                        if request
                            .expected_book_id
                            .as_deref()
                            .is_none_or(|expected| expected == book.id) =>
                    {
                        (shared_path, Ok(book))
                    }
                    Ok(_) => (
                        request.path.clone(),
                        Err("the shared workspace bytes do not match the requested book".into()),
                    ),
                    Err(error) => (request.path.clone(), Err(error)),
                };
                if result_tx
                    .send(ImportResult {
                        generation: request.generation,
                        source_path: request.path,
                        path,
                        kind: request.kind,
                        expected_book_id: request.expected_book_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                ui_wake.wake();
            }
        })
        .map_err(|error| format!("couldn't start the book importer: {error}"))?;
        Ok(Self { mailbox, results })
    }

    fn request(&self, request: ImportRequest) -> Result<(), String> {
        let (lock, wake) = &*self.mailbox;
        let mut state = lock.lock_recover();
        if state.closed {
            return Err("the book importer stopped".into());
        }
        state.pending = Some(request);
        drop(state);
        wake.notify_one();
        Ok(())
    }

    fn try_recv(&self) -> Result<ImportResult, mpsc::TryRecvError> {
        self.results.try_recv()
    }

    /// Stop the worker: it takes no more imports and drops a waiting one.
    fn close(&self) {
        let (lock, wake) = &*self.mailbox;
        let mut state = lock.lock_recover();
        state.closed = true;
        state.pending = None;
        drop(state);
        wake.notify_one();
    }
}

impl Drop for ImportWorker {
    fn drop(&mut self) {
        self.close();
    }
}

struct LoadingImport {
    generation: u64,
    path: PathBuf,
    kind: ImportKind,
    expected_book_id: Option<String>,
    /// The "Opening …" notice: progress, up while the import runs.
    card: Transient<()>,
}

/// A materialised workspace copy awaiting the UI/document-owner commit. The
/// filesystem effect happens before the CRDT advertises the shared book, so a
/// successful resource job never leaves peers with a dangling shelf row.
struct PendingShare {
    book_id: String,
    book: Book,
    path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceLifecycle {
    Share,
    Hydrate,
}

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

/// Accumulated reader wheel travel (px) that commits to one page turn.
const TURN_STEP_PX: f64 = 40.0;
/// The widest a reading page (and the settings column) grows: a readable
/// measure for the book's text at its default size.
const PAGE_MAX_W: f64 = 680.0;

/// Which screen is showing. Per-peer, never persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Library,
    Reader,
    /// The app settings screen — the font + colour-scheme managers.
    Settings,
    /// The window's search: a query view of the library and the store.
    Search,
}

/// Which manager the settings screen is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SettingsTab {
    Fonts,
    Colours,
    /// Library management: the folders watched for new books.
    Library,
    /// The reader key bindings the person chose.
    Shortcuts,
}

/// Reader commands whose single-character bindings each person can tailor.
/// Navigation keys (arrows/PageUp/PageDown) always remain available,
/// so changing a binding can never strand a keyboard reader.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReaderShortcut {
    PreviousPage,
    NextPage,
    Bookmark,
    Highlight,
    AddNote,
}

impl ReaderShortcut {
    const ALL: [Self; 5] = [
        Self::PreviousPage,
        Self::NextPage,
        Self::Bookmark,
        Self::Highlight,
        Self::AddNote,
    ];

    const fn id(self) -> &'static str {
        match self {
            Self::PreviousPage => "previous-page",
            Self::NextPage => "next-page",
            Self::Bookmark => "bookmark",
            Self::Highlight => "highlight",
            Self::AddNote => "add-note",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::PreviousPage => "Previous page",
            Self::NextPage => "Next page",
            Self::Bookmark => "Bookmark page",
            Self::Highlight => "Highlight selection",
            Self::AddNote => "Add note",
        }
    }

    const fn default_key(self) -> &'static str {
        match self {
            Self::PreviousPage => ",",
            Self::NextPage => ".",
            Self::Bookmark => "b",
            Self::Highlight => "h",
            Self::AddNote => "m",
        }
    }
}

/// An in-progress inline rename (font display name or scheme name), backed by
/// the toolkit's reusable [`ui::widget::EditableLabel`].
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
/// [`DragSelect`] click ladder: single click drags by character, double click
/// by word, and triple click by logical text block (the reader's line unit).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DragUnit {
    Char,
    Word,
    Line,
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
    /// Whether a line at flow-y `ly` with height `lh` shows in this pane. A
    /// scrolling pane shows whole lines only, so its edges never slice one.
    fn shows(&self, ly: f32, lh: f32) -> bool {
        if self.partial {
            ly >= self.win_start - 0.5 && ly + lh <= self.win_end + 0.5
        } else {
            ly >= self.win_start - 0.5 && ly < self.win_end
        }
    }
    fn screen_y(&self, ly: f32) -> f64 {
        self.y + f64::from(ly - self.win_start)
    }
}

/// What every reading column of one reader frame paints from: the laid-out
/// chapter, its annotations and page palette, the body font, and the live
/// selection and open comment group.
struct ReaderFrame<'a> {
    layout: &'a ChapterLayout,
    annos: &'a [Annotation],
    palette: &'a ReaderPalette,
    base_family: &'a FontChoice,
    base_mono: bool,
    chapter: usize,
    sel_ord: Option<(DocPos, DocPos)>,
    comment_group: Option<Uuid>,
}

/// One visible line of a pane, with the ranged highlights over its block.
#[derive(Clone, Copy)]
struct PaneLine<'a> {
    pane: &'a Pane,
    block: &'a crate::paginate::LaidBlock,
    line: &'a crate::paginate::LaidLine,
    ranged: &'a [&'a Annotation],
}

impl PaneLine<'_> {
    /// Screen y of the line's top.
    fn y(&self) -> f64 {
        self.pane.screen_y(self.line.y)
    }

    fn height(&self) -> f64 {
        f64::from(self.block.line_height)
    }
}

/// The screen x-spans of one line covered by the live selection and by each
/// of its block's ranged highlights (indexed like [`PaneLine::ranged`]).
struct LineSpans {
    sel: Option<(f64, f64)>,
    hi: Vec<Option<(f64, f64)>>,
}

/// What a pane's band pass leaves behind: the first selected band's
/// anchor, the open comment group's on-page bounds, and the topmost band
/// `(y, colour)` per highlight group, for the margin markers drawn after.
#[derive(Default)]
struct PaneBands {
    sel_anchor: SelAnchor,
    comment_box: CommentBoxBounds,
    group_top: HashMap<Uuid, (f64, HiColor)>,
}

/// An in-flight page-turn slide for the paged / two-up modes: the incoming
/// page eases in from the side it was turned from (`dir` is `+1` forward,
/// `-1` back) while fading up. Continuous scroll never uses this.
#[derive(Clone, Copy)]
struct PageTurn {
    anim: ui::motion::Presence,
    dir: f64,
}

impl PageTurn {
    /// The next page arrives from the side it lives on, on the shared axis
    /// a drill uses (`ui::motion::Drill`).
    fn start(dir: f64) -> Self {
        let mut anim = ui::motion::Presence::closed();
        anim.show(
            true,
            ui::motion::Drill::SPEC,
            ui::tokens::EffectiveMotion::default(),
            ui::tokens::MotionSource::Pointer,
            ui::time::now(),
        );
        Self { anim, dir }
    }

    /// `(horizontal offset px, opacity)` of the incoming page at `now`.
    fn visual(&self, now: Instant, page_w: f64) -> (f64, f32) {
        let p = self.anim.progress(now);
        let pose = ui::motion::Drill::pose(
            self.dir * (1.0 - p),
            page_w,
            ui::tokens::EffectiveMotion::default(),
        );
        (pose.to_drawn(Point::ZERO).x, pose.opacity)
    }

    fn done(&self, now: Instant) -> bool {
        !self.anim.is_moving(now)
    }
}

/// Everything a click or key can ask the app to do.
#[derive(Clone)]
enum Action {
    OpenBook(usize),
    Back,
    PrevPage,
    NextPage,
    SeekProgress,
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
    ShareToWorkspace(String),
    /// Remove a book (by library index) from the shelf. Its annotations +
    /// progress are kept, keyed by content id, so re-adding the file restores them.
    RemoveFromLibrary(String),
    ToggleWatchFolder(PathBuf),
    /// Keep a copy of the book catalogues on this device, or stop.
    ToggleKeepCatalogues,

    // --- settings screen: navigation -------------------------------------
    OpenSettings,
    CloseSettings,
    SetSettingsTab(SettingsTab),
    BeginShortcutCapture(ReaderShortcut),

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
    /// Enable/disable a font (whether it appears in the quick settings picker).
    ToggleFontShown(String),
    RemoveFont(String),
    OpenFontPicker,
    CloseFontPicker,
    AddSystemFont(String),
    BeginRenameFont(String),

    // --- search ------------------------------------------------------------
    /// Open the book, chapter, annotation or passage a result is anchored at.
    OpenHit(String),
    /// Get the store's book by its catalogue number, and open it.
    GetBook(u64),
    /// The chips asked for this query.
    SetQuery(ui::framework::views::QuerySpec),
    /// Open the values of a filter over its chip.
    OpenFacet(String),
    /// Close the search, back to where it began.
    CloseSearch,
}

impl BooksApp {
    /// The book currently open in the reader, if the library is non-empty.
    /// Used by [`crate::actions`] to publish the open-resource tree.
    pub(crate) fn open_book(&self) -> Option<&Book> {
        self.library.get(self.book_idx).map(Arc::as_ref)
    }

    #[cfg(test)]
    pub(crate) fn library(&self) -> &[Arc<Book>] {
        self.library.as_ref()
    }

    pub(crate) fn current_book_resource_context(&self) -> Option<crate::actions::BookResourceCtx> {
        let book = self.open_book()?;
        let block = self.page_top_block;
        Some(crate::actions::BookResourceCtx {
            book_id: book.id.clone(),
            chapter: self.chapter,
            bookmark: self.bookmark_params(block),
            page_note: books::AddNote {
                passage: Passage {
                    book: book.id.clone(),
                    chapter: self.chapter,
                    block,
                    start: 0,
                    end: 0,
                    excerpt: String::new(),
                },
                color: HiColor::Yellow,
                text: String::new(),
            },
            selection_highlight: self
                .sel
                .and_then(|selection| self.resolve_highlight_plan(selection, HiColor::Yellow)),
            selection_note: self.selection_note_draft(String::new()),
        })
    }

    /// A note on the live selection, anchored where the toolbar's Note button
    /// anchors it: the selected span of its first block.
    fn selection_note_draft(&self, text: String) -> Option<books::AddNote> {
        let (start, end) = self.sel.filter(|s| !s.is_empty())?.ordered();
        let end_ch = if end.block == start.block {
            end.ch.max(start.ch)
        } else {
            self.open_book()
                .and_then(|book| book.chapters.get(self.chapter))
                .and_then(|chapter| chapter.blocks.get(start.block))
                .map_or(start.ch, |block| {
                    crate::paginate::selectable_text(block).0.chars().count()
                })
        };
        Some(books::AddNote {
            passage: Passage {
                book: self.book_id(),
                chapter: self.chapter,
                block: start.block,
                start: start.ch,
                end: end_ch,
                excerpt: self.selection_excerpt(),
            },
            color: self.hi_color,
            text,
        })
    }

    /// Select a passage of the open book as a drag would and reveal its page.
    fn select_passage(
        &mut self,
        book_id: &str,
        chapter: usize,
        start: (usize, usize),
        end: (usize, Option<usize>),
    ) -> Result<(), String> {
        let index = self
            .library
            .iter()
            .position(|book| book.id == book_id)
            .ok_or("book is no longer in the library")?;
        let blocks = self
            .library
            .get(index)
            .and_then(|book| book.chapters.get(chapter))
            .map(|chapter| &chapter.blocks)
            .ok_or("chapter is not in this book")?;
        let chars = |block: usize| {
            blocks
                .get(block)
                .map(|b| crate::paginate::selectable_text(b).0.chars().count())
        };
        let (start_block, end_block) = (start.0, end.0);
        let (Some(start_len), Some(end_len)) = (chars(start_block), chars(end_block)) else {
            return Err("block is not in this chapter".into());
        };
        let anchor = DocPos {
            block: start_block,
            ch: start.1.min(start_len),
        };
        let head = DocPos {
            block: end_block,
            ch: end.1.unwrap_or(end_len).min(end_len),
        };
        if head.key() <= anchor.key() {
            return Err("the passage ends before it starts".into());
        }
        // The position is explicit: `persist_progress` records it, where a
        // pending `adopt_position` would take up the stored chapter instead.
        if self.book_idx != index || self.chapter != chapter {
            self.book_idx = index;
            self.chapter = chapter;
            self.page = 0;
            self.retire_layout();
        }
        self.view = View::Reader;
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.sel = Some(TextSel { anchor, head });
        self.selected_block = Some(anchor.block);
        self.pending_jump_block = Some(anchor.block);
        self.dirty = true;
        Ok(())
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag spanning unrelated \
              subsystems (library-load state, drag/selection gestures, popover \
              open/closed toggles); \
              grouping them into enums would not express any real invariant \
              between them and would touch reader, library and settings call \
              sites across this file"
)]
pub struct BooksApp {
    // --- parsed content (render cache, regenerable; not in the CRDT) -------
    // Membership snapshots share each immutable parsed book independently.
    // Adding/removing one shelf entry must never clone another EPUB's chapters.
    library: Arc<Vec<Arc<Book>>>,
    /// The library's books whose text is on another device, as their rows
    /// name them: on the shelf after [`Self::library`]'s, their shelf
    /// indices following on from it.
    elsewhere: Arc<Vec<Arc<Book>>>,
    /// Where each book's `.epub` lives, keyed by content id. The durable
    /// record is the library ([`Ebook`]); this is its cache.
    library_paths: HashMap<String, PathBuf>,
    /// Whether the persisted library has been hydrated from the document yet
    /// (loads on the first `sync`; shot/test builds pass one explicitly).
    library_loaded: bool,
    /// A shelf book, by content id, to open on the next `sync` that holds it.
    pending_open_book: Option<String>,
    /// The host's content store, where rows naming a content hash find
    /// their bytes on a device without the file, and where a book opened by
    /// its contents is kept.
    content: Option<tilo_content::ContentStore>,
    /// Whether the library is the one compiled into the app (a host without
    /// files, the browser): it opens on the book last read.
    bundled_library: bool,
    /// Whether this is a window someone uses: only a live reader watches
    /// folders, imports and reads the device's own grants.
    live: bool,
    /// The books a screenshot reads in place of files.
    sample: Option<std::rc::Rc<crate::scenarios::SampleShelf>>,
    /// Shelf rows whose bytes were looked for, found or not, so a row this
    /// device cannot read is not looked for again on every change.
    hydrated: std::collections::HashSet<String>,
    /// Books put on the shelf ahead of their row, which a state from before
    /// the row arrives must not take off it again.
    shelving: std::collections::HashSet<String>,
    /// An in-flight host file-picker request, drained once per frame in
    /// `sync`. `None` when no picker is open (the app never draws its own).
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_lifecycle: Option<ResourceLifecycle>,
    completed_resource_invocation: Option<tilo_actions::InvocationUpdate>,
    /// Callers waiting for a share or fetch to settle.
    effect_replies: Vec<(tilo_actions::InvocationId, ui::framework::EffectReply)>,
    /// The sole background parser. It owns no document state; completed books
    /// are committed only by [`Self::drain_import_worker`] on the UI thread.
    import_worker: Option<ImportWorker>,
    /// Spawn failures are deferred into an honest error only if the reader
    /// actually asks to import, rather than failing app startup outright.
    import_worker_error: Option<String>,
    import_generation: u64,
    import_loading: Option<LoadingImport>,
    pending_share: Option<PendingShare>,
    /// Why the last open failed, as a toast for a notice's lifetime.
    load_error: Transient<String>,
    /// Why the last open failed, for as long as that stands: the empty
    /// library says it in place of its hint until a book opens or another
    /// import starts. It is state, not a toast, so it keeps no time.
    open_failure: Option<String>,

    // --- taffy / paint geometry -------------------------------------------
    rect: Rect,
    dirty: bool,
    /// The settings' switches, sliding as a click turns them.
    faces: ui::widget::FaceMotions,

    // --- view state (ephemeral, per-peer) ---------------------------------
    view: View,
    book_idx: usize,
    chapter: usize,
    page: usize,
    /// Contents: the window's sidebar, docked beside the page on a wide
    /// window and a floating drawer on a narrow one.
    toc_drawer: SidebarDrawer,
    /// Annotations: the inspector at the trailing edge, docked when the page
    /// beside it keeps its room and a floating drawer otherwise.
    notes_drawer: SidebarDrawer,
    /// Height of the window's header row in the current theme, refreshed at
    /// the top of every frame so the event path hit-tests what was painted.
    header_h: f64,
    /// The page's spacing and compact breakpoint at the window's UI scale,
    /// and the height of the selection controls docked above the prose;
    /// refreshed with `header_h`.
    reader_spacing: ui::tokens::spacing::Spacing,
    reader_compact_breakpoint: f64,
    selection_hud_h: f64,
    /// Height of the page bubble in the current theme: the page foot's band
    /// is at least this tall, so the bubble it shares with the progress
    /// slider never covers text. Refreshed with `header_h`.
    bubble_h: f64,
    /// The reading-appearance button as last painted: where its popover
    /// opens from.
    type_button: Rect,
    /// Independent rail offsets.  A book can have hundreds of chapters and a
    /// reader can have years of annotations, so neither rail may silently
    /// truncate at the bottom of the viewport.
    toc_scroll: f64,
    toc_max_scroll: f64,
    toc_scroll_to_active: bool,
    /// Pitch of the first contents row as last laid out (`draw_toc`), so
    /// keyboard paging inside the rail steps by a real row at any text scale.
    toc_row_pitch: f64,
    /// Measured line-box height of the framed page's running header at the
    /// current chrome text scale, refreshed at the top of every reader frame
    /// (`draw_reader`) so the pure page geometry — which has no renderer at
    /// hand and is also consulted by hit-testing — sizes the header slot from
    /// the box the header is really painted in.
    running_header_h: f64,
    notes_scroll: f64,
    notes_max_scroll: f64,
    /// Request a bounded reveal of one annotation (from a search result or
    /// an edit affordance) on the next rail layout.
    notes_scroll_to_annotation: Option<Uuid>,
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
    /// The paginated page's leading and trailing side margins, which turn it.
    turn_margins: (Option<Rect>, Option<Rect>),
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
    /// Reading-settings popover; owns its open state, drawing, inputs, and
    /// modal hit stack, and emits settings effects for CRDT persistence here.
    reading_settings: ReadingSettings,

    /// Owns the transient page indicator's animation, drawing and hit-testing.
    reader_bubble: ReaderBubble,

    cursor: Point,
    /// Pointer tooltip derived from the same semantic label the accessibility
    /// tree exposes. Keeping this retained avoids a hand-maintained second
    /// vocabulary for the reader's icon-only and compact controls.
    hover_tooltip: Option<String>,
    /// Stable synthetic control focused by assistive technology. Pointer and
    /// keyboard activation still dispatch through the same [`Action`] path.
    semantic_focus: Option<String>,
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
    /// Effective host motion policy. Kept on the custom-painted root so its
    /// frame-demand query cannot accidentally bypass the toolkit preference.
    reduce_motion: bool,

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
    /// Library-wide snapshot used by global search. The reader-facing
    /// `annotations` cache above remains scoped to the open book.
    all_annotations: Arc<Vec<Annotation>>,
    /// Retained global-search index. Building chapter full text is linear in
    /// the library, so it is refreshed at document/materialisation boundaries
    /// and cloned cheaply into each palette session rather than rebuilt per
    /// keystroke or provider request.
    book_search: crate::search_provider::BookSearch,
    /// The window's search over the library and the store.
    query: query_view::BookQuery,
    /// A single off-UI builder. A newer snapshot supersedes its result before
    /// publication instead of mutating a live provider in place.
    search_worker: Option<SearchWorker>,
    /// Content fingerprint used only to decide whether the immutable search
    /// projection must be rebuilt. Cursor generation is separate and strictly
    /// monotonic: two same-sized catalogues can never validate one another.
    search_fingerprint: u64,
    search_revision: u64,

    // --- settings screen (ephemeral) --------------------------------------
    settings_tab: SettingsTab,
    renaming: Option<Rename>,
    /// Shared spin buttons used by the quick-reading and font-manager panels.
    number_controls: NumberControls,
    /// Modal colour editor; owns HSV state, drawing, hit regions and drags.
    color_picker: SchemeColorSheet,
    /// The default-font dropdown of the fonts manager.
    font_dropdown: HostedDropdown,
    /// The open sheet's action and close controls as last laid out, for
    /// their hover faces.
    modal_actions: Vec<Rect>,
    modal_close: Rect,
    library_prefs: crate::model::LibraryPrefs,
    watched_folders: Vec<PathBuf>,
    /// Local source paths already handed off by a watched-folder scan, keyed
    /// by the imported book identity. A successful collaborative import
    /// replaces `library_paths` with the workspace path, so this separate
    /// registry prevents re-queuing the original local file forever while
    /// still allowing removal of one book to re-enable only its own source.
    watched_sources: HashMap<PathBuf, String>,
    /// Device-local portal grants, persisted outside the collaborative doc.
    local_library: LocalLibrary,
    /// The person's own reader key bindings.
    reader_keys: crate::reader_keys::ReaderKeys,
    /// Whether this device keeps a copy of the book catalogues.
    catalogue_setting: Option<ui::framework::Setting<crate::catalogue::CatalogueCopies>>,
    /// The device's copy of Project Gutenberg's catalogue, in a live window.
    catalogue: Option<std::sync::Arc<crate::catalogue::DeviceCatalogue>>,
    /// What the copy was last told: kept or released.
    catalogue_kept: Option<bool>,
    /// The command currently awaiting a one-key local binding in Settings.
    shortcut_capture: Option<ReaderShortcut>,
    /// A short, local capture error announced through the row's accessible
    /// name as well as the visible settings UI.
    shortcut_capture_error: Option<String>,
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
    /// The chapter, block and page last recorded: a scroll records its
    /// place as it reaches another block or page, not every frame it moves.
    last_persisted: Option<(usize, usize, usize)>,
    /// The open book's stored position is still to be taken up: set when a
    /// book opens, cleared once the reader shows it there.
    adopt_position: bool,
    reading_progress: f32,
    progress_slider_track: Rect,
    progress_dragging: bool,
    pending_seek_fraction: Option<f32>,
    cur_page_count: usize,
    /// Capture-only clean page and drawers, applied after layout resolution.
    shot_drawers: Option<(bool, bool)>,
    shelf_focus_visible: bool,
    /// A book dropped anywhere on the window opens like one chosen; one
    /// dragged over it lights the shelf's add tile.
    file_open: ui::widget::FileOpenSurface,

    // --- layout cache ------------------------------------------------------
    layout: Option<ChapterLayout>,
    layouts: chapter_layouts::ChapterLayouts,
    /// Whether the layout last drawn holds the whole chapter.
    flow_complete: bool,
    /// Decoded cover artwork lifted into GPU-ready image handles, keyed by
    /// book id and built lazily on first shelf draw.
    cover_cache: HashMap<String, PenikoImage>,

    // --- per-frame hit regions --------------------------------------------
    hits: Vec<Hit>,

    /// The library's dispatcher: each on-page affordance submits the
    /// library's field edit or verb through it, exactly as the Ctrl+K
    /// palette does.
    dispatch: ui::framework::Dispatcher<crate::resource::Ebook>,
    /// The library as the framework last presented it.
    record: Rc<Ebook>,
    /// Whether this window edited the library since it was last read, so
    /// the next refresh reads it again from the document.
    record_stale: std::cell::Cell<bool>,
    /// What a search hit or an agent's `open` asked to show, shown on the
    /// next sync.
    opens: Rc<std::cell::RefCell<Vec<String>>>,
}

impl BooksApp {
    /// The live app: starts on an empty shelf and hydrates the reader's
    /// persisted library from the document on the first `sync`.
    pub fn new(
        dispatch: ui::framework::Dispatcher<crate::resource::Ebook>,
        bundled_library: bool,
    ) -> Self {
        let mut app = Self::with_library(dispatch, Vec::new());
        app.library_loaded = false;
        app.view = View::Library;
        app.bundled_library = bundled_library;
        app
    }
}

impl BooksApp {
    pub(crate) fn with_library(
        dispatch: ui::framework::Dispatcher<crate::resource::Ebook>,
        library: Vec<Book>,
    ) -> Self {
        // Open straight into the reader when there's a book; otherwise land
        // on the library so an empty shelf shows its guidance.
        let view = if library.is_empty() {
            View::Library
        } else {
            View::Reader
        };
        let library: Arc<Vec<Arc<Book>>> = Arc::new(library.into_iter().map(Arc::new).collect());
        let book_search = crate::search_provider::BookSearch::new_shared(
            Arc::clone(&library),
            Arc::new(Vec::new()),
            Arc::new(Vec::new()),
            1,
        );
        Self {
            library,
            view,
            book_search,
            search_revision: 1,
            ..Self::blank_view_state(dispatch)
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
    fn blank_view_state(dispatch: ui::framework::Dispatcher<crate::resource::Ebook>) -> Self {
        let (import_worker, import_worker_error) = match ImportWorker::new() {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            library: Arc::new(Vec::new()),
            elsewhere: Arc::new(Vec::new()),
            library_paths: HashMap::new(),
            // A caller-supplied library (tests / shots) is authoritative; the live
            // `new()` clears this so the document load runs.
            library_loaded: true,
            pending_open_book: None,
            content: None,
            bundled_library: false,
            live: true,
            sample: None,
            hydrated: std::collections::HashSet::new(),
            shelving: std::collections::HashSet::new(),
            resource_invocation: None,
            resource_lifecycle: None,
            completed_resource_invocation: None,
            effect_replies: Vec::new(),
            import_worker,
            import_worker_error,
            import_generation: 0,
            import_loading: None,
            pending_share: None,
            load_error: Transient::new(),
            open_failure: None,
            rect: Rect::ZERO,
            dirty: true,
            faces: ui::widget::FaceMotions::default(),
            view: View::Library,
            book_idx: 0,
            chapter: 0,
            page: 0,
            toc_drawer: SidebarDrawer::new(),
            notes_drawer: SidebarDrawer::new()
                .right()
                .docked_width(f64::from(Metrics::BASE.inspector_width)),
            header_h: f64::from(Metrics::BASE.header_height),
            reader_spacing: ui::tokens::spacing::Spacing::BASE,
            reader_compact_breakpoint: f64::from(Metrics::BASE.compact_breakpoint),
            selection_hud_h: ui::SelectionHud::height(&Theme::dark()),
            bubble_h: ui::status_bubble::pill_height(&Theme::dark()),
            type_button: Rect::ZERO,
            toc_scroll: 0.0,
            toc_max_scroll: 0.0,
            toc_scroll_to_active: true,
            toc_row_pitch: f64::from(ui::tokens::Metrics::BASE.panel_row_height),
            running_header_h: f64::from(ui::tokens::Typography::default().size(TextRole::Caption)),
            notes_scroll: 0.0,
            notes_max_scroll: 0.0,
            notes_scroll_to_annotation: None,
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
            turn_margins: (None, None),
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
            reading_settings: ReadingSettings::default(),
            reader_bubble: ReaderBubble::default(),
            cursor: Point::ZERO,
            hover_tooltip: None,
            semantic_focus: None,
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
            reduce_motion: false,
            hi_markers: Vec::new(),
            marker_drag: None,
            marker_group: None,
            settings: Settings::default(),
            fonts: default_fonts(),
            default_font_id: String::new(),
            schemes: default_schemes(),
            annotations: Vec::new(),
            all_annotations: Arc::new(Vec::new()),
            book_search: crate::search_provider::BookSearch::new(&[], &[]),
            query: query_view::BookQuery::hermetic(),
            search_worker: None,
            search_fingerprint: 0,
            search_revision: 0,
            settings_tab: SettingsTab::Fonts,
            renaming: None,
            number_controls: NumberControls::default(),
            color_picker: SchemeColorSheet::default(),
            font_dropdown: HostedDropdown::new("books.settings.default-font", "Default font"),
            modal_actions: Vec::new(),
            modal_close: Rect::ZERO,
            library_prefs: crate::model::LibraryPrefs::default(),
            watched_folders: Vec::new(),
            watched_sources: HashMap::new(),
            local_library: LocalLibrary::default(),
            reader_keys: crate::reader_keys::ReaderKeys::default(),
            catalogue_setting: None,
            catalogue: None,
            catalogue_kept: None,
            shortcut_capture: None,
            shortcut_capture_error: None,
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
            adopt_position: true,
            reading_progress: 0.0,
            progress_slider_track: Rect::ZERO,
            progress_dragging: false,
            pending_seek_fraction: None,
            cur_page_count: 1,
            shot_drawers: None,
            shelf_focus_visible: false,
            file_open: ui::widget::FileOpenSurface::default(),
            layout: None,
            layouts: chapter_layouts::ChapterLayouts::default(),
            flow_complete: false,
            cover_cache: HashMap::new(),
            hits: Vec::new(),
            dispatch,
            opens: Rc::default(),
            record: Rc::new(Ebook::default()),
            record_stale: std::cell::Cell::new(false),
        }
    }

    /// Screenshot-only: force the floating page bubble hover-pinned open so
    /// a single still frame captures the transient indicator. Never armed at
    /// runtime.
    fn arm_reader_bubble_for_shots(&mut self) {
        self.reader_bubble.arm_for_shots();
    }

    fn num_chapters(&self) -> usize {
        self.library
            .get(self.book_idx)
            .map_or(0, |b| b.chapters.len())
    }

    // --- Responsive layout: the contents sidebar and the annotations
    // inspector are the toolkit's drawers, docked on a wide window and
    // floating over the page on a narrow one.

    fn toc_open(&self) -> bool {
        self.toc_drawer.is_open()
    }

    fn notes_open(&self) -> bool {
        self.notes_drawer.is_open()
    }

    /// The visible, interactive portion of a rail in this frame. A floating
    /// drawer reports its animated position so a wheel during the slide
    /// still belongs to the rail rather than leaking through to page turns.
    fn toc_panel_rect(&self) -> Option<Rect> {
        let rect = self.toc_drawer.sidebar_rect();
        (self.toc_drawer.reveal() > 0.001 && rect.width() > 0.0).then_some(rect)
    }

    fn notes_panel_rect(&self) -> Option<Rect> {
        let rect = self.notes_drawer.sidebar_rect();
        (self.notes_drawer.reveal() > 0.001 && rect.width() > 0.0).then_some(rect)
    }

    /// Scroll only the rail under the pointer.  Notes are considered first:
    /// on an extremely narrow overlay they paint above the contents rail and
    /// therefore own the gesture too.
    #[allow(
        clippy::float_cmp,
        reason = "the previous value is the exact stored offset; this tests whether clamp() changed it, not an approximate geometric relation"
    )]
    fn scroll_reader_rail(&mut self, dy: f64) -> Option<bool> {
        let scroll = |value: &mut f64, max: f64| {
            let before = *value;
            *value = (*value + dy).clamp(0.0, max);
            *value != before
        };
        // A rail's list is everything under its header row.
        let header_h = self.header_h;
        let over_list =
            |rect: Rect, cursor: Point| rect.contains(cursor) && cursor.y >= rect.y0 + header_h;
        if self
            .notes_panel_rect()
            .is_some_and(|rect| over_list(rect, self.cursor))
        {
            return Some(scroll(&mut self.notes_scroll, self.notes_max_scroll));
        }
        if self
            .toc_panel_rect()
            .is_some_and(|rect| over_list(rect, self.cursor))
        {
            return Some(scroll(&mut self.toc_scroll, self.toc_max_scroll));
        }
        None
    }

    /// Page and arrow keys retain their reader meaning until a rail control
    /// has focus.  Once a contents/annotation row is focused, scrolling stays
    /// inside that rail; this prevents keyboard and pointer users from
    /// accidentally losing their place while browsing long lists.
    #[allow(
        clippy::float_cmp,
        reason = "the previous value is the exact stored offset; this tests whether clamp() changed it, not an approximate geometric relation"
    )]
    fn handle_focused_rail_key(&mut self, key: &Key) -> Option<bool> {
        let target = self.semantic_focus.as_deref()?;
        let (scroll, max, unit) = if target.starts_with("books.reader.chapter:") {
            (
                &mut self.toc_scroll,
                self.toc_max_scroll,
                self.toc_row_pitch,
            )
        } else if target.starts_with("books.annotation.") || target.starts_with("books.highlight.")
        {
            (&mut self.notes_scroll, self.notes_max_scroll, 64.0)
        } else {
            return None;
        };
        let page = (self.rect.height() * 0.8).max(unit);
        let before = *scroll;
        match key {
            Key::Named(NamedKey::ArrowDown) => *scroll += unit,
            Key::Named(NamedKey::ArrowUp) => *scroll -= unit,
            Key::Named(NamedKey::PageDown) => *scroll += page,
            Key::Named(NamedKey::PageUp) => *scroll -= page,
            Key::Named(NamedKey::Home) => *scroll = 0.0,
            Key::Named(NamedKey::End) => *scroll = max,
            _ => return None,
        }
        *scroll = (*scroll).clamp(0.0, max);
        // Consuming the key even at an edge prevents a rail-focused keypress
        // from leaking through to page navigation.
        Some(*scroll != before)
    }

    /// Resolve both drawers against the window: the contents sidebar
    /// against the whole window, the annotations inspector against what the
    /// sidebar leaves. Cheap and clock-free, so the draw and the event paths
    /// agree.
    fn resolve_drawers(&mut self) -> Rect {
        let beside_toc = self.toc_drawer.resolve(self.rect);
        self.notes_drawer.resolve(beside_toc)
    }

    fn flaps_animating(&self) -> bool {
        self.toc_drawer.is_animating() || self.notes_drawer.is_animating()
    }

    // --- library <-> cache plumbing -----------------------------------------

    /// Take up the library as the document holds it after this window's own
    /// edits of an event, ahead of the framework presenting it, then refresh
    /// the render caches. Idempotent; called from `sync` and after every
    /// handled event so the next `draw` is always consistent.
    fn refresh_from_doc(&mut self, doc: &AutoCommit) {
        if self.record_stale.replace(false) {
            self.read_record(doc);
        } else {
            self.refresh_from_record();
        }
    }

    /// Read the library from `doc` and refresh the render caches from it.
    /// Which books are on the shelf follows the library when the framework
    /// presents it ([`Self::adopt_resource_state`]).
    pub(crate) fn read_record(&mut self, doc: &AutoCommit) {
        if let Ok(record) = <Ebook as ui::tilo_resource::Resource>::read(doc) {
            self.record = Rc::new(record);
        }
        self.refresh_from_record();
    }

    /// Pull settings + annotations + shelf summaries out of the library into
    /// the render caches.
    fn refresh_from_record(&mut self) {
        let record = Rc::clone(&self.record);
        if record.settings != self.settings {
            self.settings = record.settings.clone();
        }
        let new_fonts: Vec<FontEntry> = record.fonts.values().cloned().collect();
        let new_default = record.default_font.0.clone();
        if new_fonts != self.fonts || new_default != self.default_font_id {
            self.fonts = new_fonts;
            self.default_font_id = new_default;
        }
        self.schemes = record.schemes.values().cloned().collect();
        self.library_prefs = record.prefs;

        let mut ids: Vec<String> = self.library.iter().map(|b| b.id.clone()).collect();
        let cur_id = ids.get(self.book_idx).cloned().unwrap_or_default();
        self.annotations = record.annotations_for(&cur_id);
        // Every book of the library is on the shelf, whichever device has
        // its text: its progress and notes are its synced row's.
        let here: std::collections::HashSet<String> = ids.iter().cloned().collect();
        ids.extend(
            record
                .entries
                .values()
                .filter(|entry| !here.contains(&entry.id))
                .map(|entry| entry.id.clone()),
        );

        self.shelf_fraction.clear();
        self.shelf_annos.clear();
        self.shelf_opened.clear();
        // One pass over the marks, however many books the library holds.
        let mut marks: HashMap<&str, usize> = HashMap::new();
        for mark in record.live_marks() {
            *marks.entry(mark.doc_id.as_str()).or_default() += 1;
        }
        for id in &ids {
            let p = record.progress_of(id);
            self.shelf_fraction.insert(id.clone(), p.fraction);
            self.shelf_opened.insert(id.clone(), p.opened);
            self.shelf_annos
                .insert(id.clone(), marks.get(id.as_str()).copied().unwrap_or(0));
        }
        // Keep the immutable search snapshot stable across ordinary frames.
        // Replacing this Arc only for a real change avoids allocating a
        // second annotation corpus while an unchanged provider is retained.
        let all = record.marks();
        if self.all_annotations.as_ref() != &all {
            self.all_annotations = Arc::new(all);
        }
    }

    /// Persist the live reading position for the open book (deduped so we don't
    /// churn the document when nothing moved).
    fn persist_progress(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(progress) = self.progress_to_record() {
            self.edit(doc, progress);
        }
    }

    /// Record where the reader is, from an effect that moved it: through the
    /// dispatcher, which the host applies like any invocation.
    fn record_progress(&mut self) {
        if let Some(progress) = self.progress_to_record() {
            self.dispatch.submit(progress);
        }
    }

    /// Recording where the reader is now, when it moved since it was last
    /// recorded.
    fn progress_to_record(&mut self) -> Option<ui::tilo_resource::Invoke<Ebook>> {
        if self.view != View::Reader {
            return None;
        }
        let block = self.reading_block();
        let key = (self.chapter, block, self.page);
        if self.last_persisted == Some(key) {
            return None;
        }
        let fraction = self.current_progress_fraction();
        let id = self.library.get(self.book_idx)?.id.clone();
        self.last_persisted = Some(key);
        let progress = Ebook::RECORD_PROGRESS.with(books::RecordProgress {
            book: id,
            chapter: self.chapter,
            block,
            fraction,
            opened: false,
        });
        // A participant the room does not let change the library (a guest
        // who may only view) reads on without their place being sent: the
        // room would refuse it on every page turn.
        self.dispatch.may_invoke(&progress).then_some(progress)
    }

    /// The first block on the page the reader is at: what the reading
    /// position records. Until a pending move is laid out it is the start
    /// of the chapter; the next frame records the block it lands on.
    fn reading_block(&self) -> usize {
        if let Some(block) = self.pending_jump_block {
            return block;
        }
        let Some(layout) = self
            .layout
            .as_ref()
            .filter(|layout| layout.key.chapter == self.chapter)
            .filter(|_| !self.pending_last_page && self.pending_seek_fraction.is_none())
        else {
            return 0;
        };
        let top = if self.settings.mode.is_continuous() {
            self.scroll_y as f32
        } else {
            layout.pages.get(self.page).copied().unwrap_or(0.0)
        };
        layout
            .blocks
            .iter()
            .find(|b| b.lines.iter().any(|l| l.y >= top - 0.5))
            .map_or(0, |b| b.index)
    }

    fn current_progress_fraction(&self) -> f32 {
        let chapters = self.num_chapters().max(1) as f32;
        if let Some(within) = self.pending_seek_fraction {
            return ((self.chapter as f32 + within) / chapters).clamp(0.0, 1.0);
        }
        let within = if self.settings.mode.is_continuous() {
            let (total, viewport) = self.flow_extent;
            if total > 0.0 {
                ((self.scroll_y + viewport) / total).clamp(0.0, 1.0) as f32
            } else {
                0.0
            }
        } else {
            (self.page as f32 + self.settings.mode.page_stride() as f32)
                / self.cur_page_count.max(1) as f32
        };
        ((self.chapter as f32 + within.clamp(0.0, 1.0)) / chapters).clamp(0.0, 1.0)
    }

    /// Where the reader is in the book for the progress slider: the start
    /// of the page on screen (or the scroll offset), so the knob and a seek
    /// to its value agree.
    fn slider_position(&self, chapters: usize) -> f32 {
        let chapters = chapters.max(1) as f32;
        if let Some(within) = self.pending_seek_fraction {
            return ((self.chapter as f32 + within) / chapters).clamp(0.0, 1.0);
        }
        let within = if self.settings.mode.is_continuous() {
            let (total, viewport) = self.flow_extent;
            let max = total - viewport;
            if max > 0.0 {
                (self.scroll_y / max).clamp(0.0, 1.0) as f32
            } else {
                0.0
            }
        } else {
            self.page as f32 / self.cur_page_count.max(1) as f32
        };
        ((self.chapter as f32 + within.clamp(0.0, 1.0)) / chapters).clamp(0.0, 1.0)
    }

    fn on_seek_progress(&mut self, fraction: f32) {
        let fraction = fraction.clamp(0.0, 1.0);
        let chapter_count = self.num_chapters();
        if chapter_count == 0 {
            return;
        }
        let position = fraction * chapter_count as f32;
        self.chapter = (position.floor() as usize).min(chapter_count - 1);
        self.page = 0;
        self.scroll_y = 0.0;
        self.pending_seek_fraction = Some(if fraction >= 1.0 {
            1.0
        } else {
            position.fract()
        });
        self.pending_last_page = false;
        self.pending_jump_block = None;
        self.pending_scroll_end = false;
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.retire_layout();
        self.toc_scroll_to_active = true;
        self.reading_progress = fraction;
    }

    fn progress_fraction_at(&self, point: Point) -> f32 {
        if self.progress_slider_track.width() <= 0.0 {
            return self.reading_progress;
        }
        ((point.x - self.progress_slider_track.x0) / self.progress_slider_track.width())
            .clamp(0.0, 1.0) as f32
    }

    fn accessible_progress_value(value: &str) -> Option<f32> {
        let value = value
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f32>()
            .ok()?;
        value.is_finite().then(|| {
            if value.abs() > 1.0 {
                value / 100.0
            } else {
                value
            }
            .clamp(0.0, 1.0)
        })
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
    /// sync, shelving any new book in a watched folder first. The shelf is
    /// home; a bundled library opens on the book last read.
    fn ensure_library_loaded(&mut self, doc: &AutoCommit) {
        if self.library_loaded {
            return;
        }
        self.library_loaded = true;

        if let Ok(record) = <Ebook as ui::tilo_resource::Resource>::read(doc) {
            self.record = Rc::new(record);
        }
        let found_paths = HashMap::new();
        Arc::make_mut(&mut self.library).clear();
        self.library_paths.clear();
        self.hydrated.clear();
        let entries: Vec<crate::model::LibraryEntry> =
            self.record.entries.values().cloned().collect();
        self.hydrate(&entries, &found_paths);

        if self.pending_open_book.is_none() && self.bundled_library {
            self.pending_open_book = self.record.most_recently_opened();
        }
        self.view = View::Library;
    }

    /// Put the shelf rows in `entries` this device can read on the shelf:
    /// the sample shelf in a screenshot, else the files and content the rows
    /// name. `found` are watched-folder files not yet remembered.
    fn hydrate(
        &mut self,
        entries: &[crate::model::LibraryEntry],
        found: &HashMap<String, PathBuf>,
    ) {
        self.hydrated
            .extend(entries.iter().map(|entry| entry.id.clone()));
        let loaded: Vec<content::LoadedBook> = if let Some(sample) = &self.sample {
            entries
                .iter()
                .filter_map(|entry| sample.book(&entry.id))
                .map(|(book, path)| content::LoadedBook {
                    book: book.clone(),
                    path: path.clone(),
                })
                .collect()
        } else {
            let mut paths = self.local_library.books.clone();
            for (id, path) in found {
                paths.entry(id.clone()).or_insert_with(|| path.clone());
            }
            content::hydrate_library(entries, &paths, self.content.as_ref())
        };
        for loaded in loaded {
            if self.library.iter().any(|book| book.id == loaded.book.id) {
                continue;
            }
            self.library_paths
                .insert(loaded.book.id.clone(), loaded.path);
            Arc::make_mut(&mut self.library).push(Arc::new(loaded.book));
        }
        self.dirty = true;
    }

    /// Open the book a launch, an effect or a bundled start asked for, once
    /// the shelf holds it.
    fn open_pending_book(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(id) = self.pending_open_book.as_deref() else {
            return;
        };
        let Some(index) = self.library.iter().position(|book| book.id == id) else {
            return;
        };
        self.pending_open_book = None;
        self.open_book_at(doc, index);
    }

    /// Open the book at library index `i` in the reader, restoring where the
    /// reader left off and stamping the open time for the "recently opened" sort.
    fn open_book_at(&mut self, doc: &mut ui::widget::Edits<'_>, i: usize) {
        if self.library.is_empty() {
            return;
        }
        self.book_idx = i.min(self.library.len() - 1);
        self.view = View::Reader;
        let Some(id) = self.library.get(self.book_idx).map(|b| b.id.clone()) else {
            return;
        };
        let p = self.record.progress_of(&id);
        let nchap = self.num_chapters().saturating_sub(1);
        self.chapter = p.chapter.min(nchap);
        self.page = 0;
        self.pending_jump_block = Some(p.block);
        self.edit(
            doc,
            Ebook::RECORD_PROGRESS.with(books::RecordProgress {
                book: id,
                chapter: p.chapter,
                block: p.block,
                fraction: p.fraction,
                opened: true,
            }),
        );
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.retire_layout();
        self.last_persisted = None;
        self.adopt_position = true;
        self.toc_scroll_to_active = true;
        self.notes_scroll_to_annotation = None;
    }

    /// Put one EPUB on the latest-wins worker mailbox. This only changes local
    /// loading state; all document mutation happens in [`Self::drain_import_worker`].
    fn begin_import(&mut self, path: PathBuf, kind: ImportKind) -> Result<(), String> {
        self.begin_import_expected(path, kind, None)
    }

    fn begin_import_expected(
        &mut self,
        path: PathBuf,
        kind: ImportKind,
        expected_book_id: Option<String>,
    ) -> Result<(), String> {
        self.begin_import_from(path, kind, expected_book_id, None)
    }

    /// Fetch the store's book `id` and open it like a book picked from
    /// disk: the loading line, the shelf row, the reader.
    pub(crate) fn begin_get_from_store(&mut self, id: u64) -> Result<(), String> {
        self.begin_import_from(
            crate::store::fetched_path(id),
            ImportKind::Open,
            None,
            Some(crate::store::book_url(id)),
        )
    }

    fn begin_import_from(
        &mut self,
        path: PathBuf,
        kind: ImportKind,
        expected_book_id: Option<String>,
        fetch: Option<String>,
    ) -> Result<(), String> {
        // A background shelf refresh must never replace an explicit user open.
        // Watch requests coalesce with one another; after a successful receipt
        // we schedule the next unseen file.
        if self.import_loading.is_some() && kind == ImportKind::Watch {
            return Ok(());
        }
        let generation = self
            .import_generation
            .checked_add(1)
            .ok_or_else(|| "book import generation exhausted; restart Books".to_string())?;
        let worker = self.import_worker.as_ref().ok_or_else(|| {
            self.import_worker_error
                .clone()
                .unwrap_or_else(|| "the book importer is unavailable".into())
        })?;
        worker.request(ImportRequest {
            generation,
            path: path.clone(),
            kind,
            expected_book_id: expected_book_id.clone(),
            fetch,
            #[cfg(test)]
            hold: None,
        })?;
        self.import_generation = generation;
        self.import_loading = Some(LoadingImport {
            generation,
            path,
            kind,
            expected_book_id,
            card: Transient::showing_progress(()),
        });
        self.clear_failure();
        self.dirty = true;
        Ok(())
    }

    /// An EPUB dropped on the window opens like one chosen in the picker.
    fn open_dropped(&mut self, path: PathBuf) {
        let epub = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"));
        if !epub {
            self.report_failure(format!(
                "Couldn't open {}: only EPUB books can be added",
                path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned()
                )
            ));
        } else if let Err(error) = self.begin_import(path, ImportKind::Open) {
            self.settle_import_failure(error, "ebook.importer_unavailable");
        }
    }

    fn settle_import_failure(&mut self, message: String, code: &str) {
        self.import_loading = None;
        self.report_failure(message.clone());
        if let Some(invocation) = self.resource_invocation.take() {
            self.completed_resource_invocation = Some(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: code.into(),
                    message,
                    details: None,
                    retryable: false,
                },
            });
        }
        self.dirty = true;
    }

    /// Accept worker results on the UI thread. Older generations have been
    /// superseded by a later request and are discarded before they can mutate
    /// the shelf, reader, or CRDT.
    /// Open a book chosen by contents: parsed here (a page has no importer
    /// thread) and shelved as the importer would.
    /// Put a book on the shelf ahead of its row, read from `path` from now
    /// on, and return where it is.
    fn shelve(&mut self, book: Book, path: PathBuf) -> usize {
        let id = book.id.clone();
        self.hydrated.insert(id.clone());
        self.shelving.insert(id.clone());
        self.library_paths.insert(id.clone(), path);
        let book = Arc::new(book);
        if let Some(index) = self.library.iter().position(|b| b.id == id) {
            if let Some(slot) = Arc::make_mut(&mut self.library).get_mut(index) {
                *slot = book;
            }
            index
        } else {
            Arc::make_mut(&mut self.library).push(book);
            self.library.len() - 1
        }
    }

    /// Shelve and open a book an effect placed, through the library's
    /// action, or say why it could not be opened.
    fn finish_opening(
        &mut self,
        name: &str,
        placed: Result<content::Placed, String>,
        cx: &ui::framework::EffectCx<crate::resource::Ebook, Self>,
    ) -> ui::framework::EffectAnswer {
        self.dirty = true;
        match placed {
            Ok(placed) => {
                cx.submit(crate::resource::shelve(&placed.entry));
                self.pending_open_book = Some(placed.entry.id);
                self.shelve(placed.book, placed.path);
                self.clear_failure();
                Ok(ui::framework::Effected::done())
            }
            Err(error) => {
                let message = format!("Couldn’t open {name}: {error}");
                self.report_failure(message.clone());
                Err(message)
            }
        }
    }

    /// Ask the person for a book with the host's chooser, then place it
    /// where the library reads it again and open it. A cancelled choice
    /// opens nothing.
    pub(crate) fn choose_book(
        cx: &mut ui::framework::EffectCx<crate::resource::Ebook, Self>,
    ) -> ui::framework::Effected {
        cx.choose_file(book_open_request(), |app: &mut Self, chosen, cx| {
            let Some(file) = chosen else {
                return Ok(ui::framework::Effected::done());
            };
            let store = app.content.clone();
            let name = file.name;
            Ok(cx.thread(
                move |_| content::place_chosen_book(&file.bytes, store.as_ref()),
                move |app: &mut Self, placed, cx| app.finish_opening(&name, placed, cx),
            ))
        })
    }

    /// Open the book whose bytes the content store holds as `hash`: parsed
    /// off the UI thread, shelved through the library's action and opened.
    /// What a launch on a book takes.
    pub(crate) fn open_stored(
        &self,
        hash: String,
        name: String,
        cx: &mut ui::framework::EffectCx<crate::resource::Ebook, Self>,
    ) -> ui::framework::EffectAnswer {
        let store = self
            .content
            .clone()
            .ok_or_else(|| "this device keeps no book contents".to_string())?;
        Ok(cx.thread(
            move |_| {
                let bytes = store
                    .read(&hash)
                    .ok_or_else(|| "its contents are not on this device".to_string())?;
                content::open_handed_book(&bytes, None).map(|mut handed| {
                    handed.content = Some(hash);
                    content::placed(handed)
                })
            },
            move |app: &mut Self, placed, cx| app.finish_opening(&name, placed, cx),
        ))
    }

    fn drain_import_worker(&mut self, doc: &mut ui::widget::Edits<'_>) {
        while let Some(outcome) = self.import_worker.as_ref().map(ImportWorker::try_recv) {
            let completed = match outcome {
                Ok(completed) => completed,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.import_worker = None;
                    if self.import_loading.is_some() {
                        self.settle_import_failure(
                            "The book importer stopped.".into(),
                            "ebook.importer_stopped",
                        );
                    }
                    break;
                }
            };
            let current = self.import_loading.as_ref().is_some_and(|loading| {
                let source_matches = loading.path == completed.source_path;
                loading.generation == completed.generation
                    && source_matches
                    && loading.kind == completed.kind
                    && loading.expected_book_id == completed.expected_book_id
            });
            if !current {
                continue;
            }
            self.import_loading = None;
            match completed.result {
                Ok(book) => {
                    let (book, entry) = content::prepare_imported_book(&completed.path, book);
                    self.edit(doc, crate::resource::shelve(&entry));
                    let id = book.id.clone();
                    self.local_library.remember_book(&id, &completed.path);
                    self.local_library.save();
                    self.library_paths.insert(id.clone(), completed.path);
                    let index = if let Some(index) = self.library.iter().position(|b| b.id == id) {
                        index
                    } else {
                        Arc::make_mut(&mut self.library).push(Arc::new(book));
                        self.library.len() - 1
                    };
                    self.clear_failure();
                    if completed.kind == ImportKind::Open {
                        self.open_book_at(doc, index);
                    }
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.completed_resource_invocation =
                            Some(tilo_actions::InvocationUpdate::Succeeded {
                                invocation,
                                output: tilo_actions::ActionOutput::default(),
                            });
                    }
                    // A watched-folder refresh advances one book at a time.
                    // A failed file is not immediately retried in a loop.
                    if completed.kind == ImportKind::Watch {
                        self.watched_sources.insert(completed.source_path, id);
                        self.rescan_watched(doc);
                    }
                }
                Err(error) => {
                    let name = completed
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("book");
                    self.report_failure(format!("Couldn’t open {name}: {error}"));
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.completed_resource_invocation =
                            Some(tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "ebook.open_failed".into(),
                                    message: self
                                        .load_error
                                        .as_deref()
                                        .unwrap_or_default()
                                        .to_owned(),
                                    details: None,
                                    retryable: false,
                                },
                            });
                    }
                }
            }
            self.dirty = true;
        }
    }

    #[cfg(test)]
    /// Queue an import whose parse waits until the test lets it go. Returns a
    /// receiver that fires once the worker has taken the request, and the
    /// sender that releases it (dropping it releases it too).
    fn begin_import_held(
        &mut self,
        path: &std::path::Path,
        kind: ImportKind,
    ) -> Result<(std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>), String> {
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let generation = self
            .import_generation
            .checked_add(1)
            .ok_or_else(|| "book import generation exhausted; restart Books".to_string())?;
        let worker = self
            .import_worker
            .as_ref()
            .ok_or_else(|| "the book importer is unavailable".to_string())?;
        worker.request(ImportRequest {
            generation,
            path: path.to_path_buf(),
            kind,
            expected_book_id: None,
            fetch: None,
            hold: Some((entered_tx, release_rx)),
        })?;
        self.import_generation = generation;
        self.import_loading = Some(LoadingImport {
            generation,
            path: path.to_path_buf(),
            kind,
            expected_book_id: None,
            card: Transient::showing_progress(()),
        });
        Ok((entered, release))
    }

    #[cfg(test)]
    fn import_and_open_for_test(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
    ) {
        let book = content::parse_path_detailed(path).expect("test EPUB parses");
        let (book, entry) = content::prepare_imported_book(path, book);
        self.edit(doc, crate::resource::shelve(&entry));
        let id = book.id.clone();
        self.local_library.remember_book(&id, path);
        self.library_paths.insert(id.clone(), path.to_path_buf());
        let index = if let Some(index) = self.library.iter().position(|b| b.id == id) {
            index
        } else {
            Arc::make_mut(&mut self.library).push(Arc::new(book));
            self.library.len() - 1
        };
        self.open_book_at(doc, index);
    }

    /// Queue at most one unseen watched EPUB. The work is deferred so focus
    /// events remain responsive even for a large library.
    fn rescan_watched(&mut self, _doc: &AutoCommit) {
        if self.import_loading.is_some() {
            return;
        }
        let known: std::collections::HashSet<PathBuf> = self
            .library_paths
            .values()
            .cloned()
            .chain(self.watched_sources.keys().cloned())
            .collect();
        if let Some(path) = content::watched_epub_paths(&self.watched_folders)
            .into_iter()
            .find(|path| !known.contains(path))
            && let Err(error) = self.begin_import(path, ImportKind::Watch)
        {
            self.settle_import_failure(error, "ebook.importer_unavailable");
        }
    }

    // --- Action dispatch (event side — has the document) --------------------

    fn dispatch(&mut self, action: Action, doc: &mut ui::widget::Edits<'_>) {
        match action {
            Action::OpenBook(i) if i >= self.library.len() => {
                // A book whose text is elsewhere: fetched, then opened where
                // its reader left off.
                if let Some(book) = self.shelf_book(i) {
                    let place = crate::resource::book_place_resource(&book.id);
                    self.open_hit(&place);
                }
            }
            Action::OpenBook(i) => {
                self.open_book_at(doc, i);
            }
            Action::OpenHit(anchor) => self.open_hit(&anchor),
            Action::GetBook(id) => self.get_from_store(id),
            Action::SetQuery(query) => self.on_set_query(&query),
            Action::OpenFacet(key) => self.open_facet(&key),
            Action::CloseSearch => self.close_query(),
            Action::Back => self.on_back(),
            Action::NextPage => self.on_next_page(),
            Action::PrevPage => self.on_prev_page(),
            Action::SeekProgress => self.on_seek_progress(self.progress_fraction_at(self.cursor)),
            Action::SetMode(mode) => self.on_set_mode(mode, doc),
            Action::GotoChapter(c) => self.on_goto_chapter(c),
            Action::ToggleToc => self.on_toggle_toc(),
            Action::ToggleNotes => self.on_toggle_notes(),
            Action::ToggleTypePopover => self.reading_settings.toggle(),
            Action::DismissOverlay => self.on_dismiss_overlay(),
            Action::Swallow => {}
            Action::ToggleBookmark => self.toggle_bookmark(doc),
            Action::FontInc => self.bump_setting(doc, |s| {
                s.font_scale = (s.font_scale + 0.1)
                    .clamp(Settings::READING_SCALE_MIN, Settings::READING_SCALE_MAX);
            }),
            Action::FontDec => self.bump_setting(doc, |s| {
                s.font_scale = (s.font_scale - 0.1)
                    .clamp(Settings::READING_SCALE_MIN, Settings::READING_SCALE_MAX);
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
            Action::CloseComments => self.on_close_comments(),
            Action::DeleteHighlight(group) => self.on_delete_highlight(group, doc),
            Action::AddComment(group) => self.on_add_comment(group, doc),
            Action::EditComment(id) => self.on_edit_comment(id),
            Action::DeleteComment(id) => self.on_delete_comment(id, doc),
            Action::OpenViaPortal => self
                .dispatch
                .effect(&crate::resource::BooksEffect::OpenBook),
            Action::ShareToWorkspace(id) => self.on_share_to_workspace(&id, doc),
            Action::RemoveFromLibrary(id) => self.on_remove_from_library(&id, doc),
            Action::ToggleWatchFolder(folder) => self.on_toggle_watch_folder(&folder, doc),
            Action::ToggleKeepCatalogues => self.on_toggle_keep_catalogues(),

            // --- settings screen: navigation -----------------------------
            Action::OpenSettings => self.on_open_settings(),
            Action::CloseSettings => self.on_close_settings(doc),
            Action::SetSettingsTab(tab) => self.on_set_settings_tab(tab, doc),
            Action::BeginShortcutCapture(shortcut) => {
                self.shortcut_capture = Some(shortcut);
                self.shortcut_capture_error = None;
            }

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
            Action::ToggleFontShown(id) => self.on_toggle_font_shown(&id, doc),
            Action::RemoveFont(id) => self.on_remove_font(&id, doc),
            Action::OpenFontPicker => self.on_open_font_picker(),
            Action::CloseFontPicker => self.font_picker = false,
            Action::AddSystemFont(family) => self.on_add_system_font(&family, doc),
            Action::BeginRenameFont(id) => self.on_begin_rename_font(id, doc),
        }
    }

    fn dispatch_reading_settings(
        &mut self,
        action: ReadingSettingsAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match action {
            ReadingSettingsAction::Swallow => {}
            ReadingSettingsAction::Dismiss => self.reading_settings.close(),
            ReadingSettingsAction::OpenManager => self.dispatch(Action::OpenSettings, doc),
            ReadingSettingsAction::SetFontScale(value) => {
                self.bump_setting(doc, |settings| settings.font_scale = value);
            }
            ReadingSettingsAction::SetLineSpacing(value) => {
                self.bump_setting(doc, |settings| settings.line_spacing = value);
            }
            ReadingSettingsAction::SetMode(mode) => self.dispatch(Action::SetMode(mode), doc),
            ReadingSettingsAction::SetComposition(composition) => {
                self.dispatch(Action::SetComposition(composition), doc);
            }
            ReadingSettingsAction::SetFont(id) => self.dispatch(Action::SetFont(id), doc),
            ReadingSettingsAction::SetReadTheme(theme) => {
                self.dispatch(Action::SetReadTheme(theme), doc);
            }
            ReadingSettingsAction::SetScheme(id) => self.dispatch(Action::SetScheme(id), doc),
        }
    }

    fn on_back(&mut self) {
        self.view = View::Library;
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.reading_settings.close();
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
        } else if self.flow_complete && self.chapter + 1 < self.num_chapters() {
            self.retire_layout();
            self.chapter += 1;
            self.page = 0;
            self.selected_block = None;
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
            self.retire_layout();
        }
        if (self.chapter, self.page) != before {
            // The new page eases in from the left — a backward turn.
            self.page_turn = Some(PageTurn::start(-1.0));
        }
    }

    fn on_set_mode(&mut self, mode: ReadMode, doc: &mut ui::widget::Edits<'_>) {
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
        }
    }

    fn on_goto_chapter(&mut self, c: usize) {
        self.chapter = c.min(self.num_chapters().saturating_sub(1));
        self.page = 0;
        self.scroll_y = 0.0;
        // A chapter destination supersedes passage and deferred navigation
        // from the previous chapter, including an unresolved progress seek.
        self.pending_last_page = false;
        self.pending_jump_block = None;
        self.pending_scroll_end = false;
        self.pending_seek_fraction = None;
        self.pending_sel_edge = None;
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.retire_layout();
        self.toc_scroll_to_active = true;
    }

    fn on_toggle_toc(&mut self) {
        self.toc_drawer.toggle();
        if self.toc_drawer.is_open() {
            self.toc_scroll_to_active = true;
        }
    }

    fn on_toggle_notes(&mut self) {
        self.notes_drawer.toggle();
    }

    fn on_dismiss_overlay(&mut self) {
        self.reading_settings.close();
        if self.toc_drawer.is_overlay() && self.toc_open() {
            self.toc_drawer.close();
        }
        if self.notes_drawer.is_overlay() && self.notes_open() {
            self.notes_drawer.close();
        }
    }

    fn on_cycle_font(&mut self, doc: &mut ui::widget::Edits<'_>) {
        // Cycle through the fonts offered in quick settings.
        let shown = self.shown_fonts();
        if !shown.is_empty() {
            let cur = self.active_font().id;
            let idx = shown.iter().position(|f| f.id == cur).unwrap_or(0);
            if let Some(next) = shown.get((idx + 1) % shown.len()).map(|f| f.id.clone()) {
                self.bump_setting(doc, |s| s.font_id = next);
            }
        }
    }

    fn on_set_color(&mut self, c: HiColor, doc: &mut ui::widget::Edits<'_>) {
        self.hi_color = c;
        // Picking the same colour again clears it; a different colour
        // replaces it. Resolves to the same action the palette's
        // "Highlight selection" dispatches — one source of truth.
        if let Some(sel) = self.sel.filter(|s| !s.is_empty()) {
            if let Some(plan) = self.resolve_highlight_plan(sel, c) {
                self.edit(doc, Ebook::HIGHLIGHT.with(plan));
            }
            self.clear_selection();
        }
    }

    fn on_add_note(&mut self, doc: &mut ui::widget::Edits<'_>) {
        // A note anchors to the live text selection; the composer
        // floats on-page near the passage rather than a separate screen.
        if let Some(draft) = self.selection_note_draft(String::new()) {
            let before = self.annotation_ids();
            self.edit(doc, Ebook::ADD_NOTE.with(draft));
            if let Some(id) = self.added_annotation(doc.doc(), &before) {
                self.composing = Some(id);
                self.note_composer.open(id, Some(&self.dispatch));
            }
        }
    }

    fn on_copy_selection(&self) {
        // Selection stays put so the reader can also highlight or note it.
        let text = self.selection_clipboard_text();
        if !text.is_empty() {
            ui::clipboard_copy(&text);
        }
    }

    fn on_edit_note(&mut self, id: Uuid) {
        if self
            .annotations
            .iter()
            .any(|a| a.id == id && a.kind == AnnoKind::Note)
        {
            self.composing = Some(id);
            self.note_composer.open(id, Some(&self.dispatch));
            self.notes_drawer.open();
            self.notes_scroll_to_annotation = Some(id);
        }
    }

    fn on_jump_to(&mut self, id: Uuid) {
        if let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() {
            self.chapter = a.chapter.min(self.num_chapters().saturating_sub(1));
            self.pending_jump_block = Some(a.block);
            self.clear_selection();
            self.comment_overlay.close();
            self.retire_layout();
            self.notes_scroll_to_annotation = Some(id);
        }
    }

    fn on_delete_annotation(&mut self, id: Uuid, doc: &mut ui::widget::Edits<'_>) {
        if self.composing == Some(id) {
            self.composing = None;
            self.note_composer.close();
        }
        self.edit(
            doc,
            Ebook::DELETE_ANNOTATION.with(books::DeleteAnnotation { annotation: id }),
        );
    }

    fn on_open_comments(&mut self, group: Uuid) {
        // The overlay owns interaction while it is up: drop any live
        // selection so its toolbar doesn't fight the popover.
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.open(group);
    }

    fn on_close_comments(&mut self) {
        self.comment_overlay.finish_editing();
        self.comment_overlay.close();
    }

    fn on_delete_highlight(&mut self, group: Uuid, doc: &mut ui::widget::Edits<'_>) {
        self.edit(
            doc,
            Ebook::DELETE_HIGHLIGHT.with(books::DeleteHighlight { highlight: group }),
        );
        if self.comment_overlay.group() == Some(group) {
            self.comment_overlay.close();
        }
        self.composing = None;
        self.note_composer.close();
    }

    fn on_add_comment(&mut self, group: Uuid, doc: &mut ui::widget::Edits<'_>) {
        // The library anchors it to the highlight's first piece, so it
        // sorts with the range.
        self.comment_overlay.finish_editing();
        let highlighted = self
            .annotations
            .iter()
            .any(|a| a.kind == AnnoKind::Highlight && a.group == group);
        if highlighted {
            let before = self.annotation_ids();
            self.edit(
                doc,
                Ebook::ADD_COMMENT.with(books::AddComment {
                    highlight: group,
                    text: String::new(),
                }),
            );
            if let Some(id) = self.added_annotation(doc.doc(), &before) {
                self.comment_overlay.open(group);
                self.comment_overlay.begin_editing(id, Some(&self.dispatch));
            }
        }
    }

    fn on_edit_comment(&mut self, id: Uuid) {
        if self
            .annotations
            .iter()
            .any(|annotation| annotation.id == id && annotation.kind == AnnoKind::Note)
        {
            self.comment_overlay.begin_editing(id, Some(&self.dispatch));
        }
    }

    fn on_delete_comment(&mut self, id: Uuid, doc: &mut ui::widget::Edits<'_>) {
        if self.comment_overlay.editing_id() == Some(id) {
            self.comment_overlay.finish_editing();
        }
        self.edit(
            doc,
            Ebook::DELETE_ANNOTATION.with(books::DeleteAnnotation { annotation: id }),
        );
    }

    fn begin_share_book(&mut self, id: &str) -> Result<(), String> {
        if self.pending_share.is_some() || self.resource_invocation.is_some() {
            return Err("another book lifecycle operation is in progress".into());
        }
        let Some(src) = self.library_paths.get(id).cloned() else {
            return Err("this book is unavailable on this device".into());
        };
        if is_shared(&src) {
            return Err("this book is already shared with the workspace".into());
        }
        let (book, path) = content::parse_shared_path_detailed(&src)?;
        if book.id != id {
            return Err("the local source no longer matches this book".into());
        }
        if !is_shared(&path) {
            return Err("a collaborative workspace is unavailable on this device".into());
        }
        self.pending_share = Some(PendingShare {
            book_id: id.to_string(),
            book,
            path,
        });
        Ok(())
    }

    /// Queue a capability-safe hydrate of a remotely advertised book. The
    /// replicated id determines the sole workspace path; the importer verifies
    /// the parsed content id before it can become a local shelf entry.
    fn begin_hydrate_shared_book(
        &mut self,
        id: &str,
        entries: &[crate::model::LibraryEntry],
    ) -> Result<(), String> {
        if self.pending_share.is_some()
            || self.resource_invocation.is_some()
            || self.import_loading.is_some()
        {
            return Err("another book lifecycle operation is in progress".into());
        }
        let advertised = entries.iter().any(|entry| entry.id == id && entry.shared);
        if !advertised {
            return Err("this shared book is no longer available in the workspace".into());
        }
        self.begin_import_expected(
            content::workspace_book_path(id),
            ImportKind::Open,
            Some(id.to_string()),
        )
    }

    fn commit_pending_share(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(pending) = self.pending_share.take() else {
            return;
        };
        let (book, entry) = content::prepare_imported_book(&pending.path, pending.book);
        self.edit(doc, crate::resource::shelve(&entry));
        if book.id == pending.book_id {
            self.local_library
                .remember_book(&pending.book_id, &pending.path);
            self.library_paths.insert(pending.book_id, pending.path);
            self.local_library.save();
            self.clear_failure();
        } else {
            self.report_failure("The shared copy no longer matches this book.".into());
        }
        if let Some(invocation) = self.resource_invocation.take() {
            self.completed_resource_invocation = Some(match self.load_error.as_deref() {
                Some(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "ebook.share_failed".into(),
                        message: message.to_owned(),
                        details: None,
                        retryable: true,
                    },
                },
                None => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
            });
        }
        self.dirty = true;
    }

    fn on_share_to_workspace(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        if let Err(error) = self.begin_share_book(id) {
            self.report_failure(error);
            return;
        }
        self.commit_pending_share(doc);
    }

    fn on_remove_from_library(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        let Some(i) = self.library.iter().position(|book| book.id == id) else {
            return;
        };
        let removed = Arc::make_mut(&mut self.library).remove(i);
        self.library_paths.remove(&removed.id);
        // A removed watched book is eligible to be discovered again. Keep the
        // watch registry an optimisation, never a hidden permanent exclusion;
        // importantly, do not discard deduplication for every other watched
        // source when a single shelf row is removed.
        self.watched_sources.retain(|_, id| id != &removed.id);
        self.local_library.forget_book(&removed.id);
        self.local_library.save();
        self.cover_cache.remove(&removed.id);
        self.edit(
            doc,
            Ebook::REMOVE_FROM_LIBRARY.with(books::RemoveFromLibrary {
                book: removed.id.clone(),
            }),
        );
        // Keep the reader pointed at a valid book after the shelf shrinks.
        self.book_idx = self.book_idx.min(self.library.len().saturating_sub(1));
        self.shelf_cursor = self.shelf_cursor.min(self.library.len().saturating_sub(1));
        if self.library.is_empty() {
            self.view = View::Library;
        }
        self.retire_layout();
    }

    fn on_toggle_watch_folder(&mut self, folder: &std::path::Path, doc: &AutoCommit) {
        self.local_library.toggle_watch(folder);
        self.watched_folders
            .clone_from(&self.local_library.watched_folders);
        self.local_library.save();
        // Watching a folder pulls in any books already sitting in it.
        self.rescan_watched(doc);
    }

    fn on_open_settings(&mut self) {
        self.view = View::Settings;
        self.reading_settings.close();
        // Numeric controls retain their own edit state and commit on blur.
        self.font_picker = false;
        self.color_picker.cancel();
        self.font_dropdown.close();
        self.editing_scheme = None;
        self.renaming = None;
        self.shortcut_capture = None;
        self.shortcut_capture_error = None;
        self.settings_scroll = 0.0;
    }

    fn on_close_settings(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.commit_rename(doc);
        self.font_picker = false;
        self.color_picker.cancel();
        self.font_dropdown.close();
        self.editing_scheme = None;
        self.shortcut_capture = None;
        self.shortcut_capture_error = None;
        self.view = if self.library.is_empty() {
            View::Library
        } else {
            View::Reader
        };
    }

    fn on_set_settings_tab(&mut self, tab: SettingsTab, doc: &mut ui::widget::Edits<'_>) {
        self.commit_rename(doc);
        self.font_dropdown.close();
        self.color_picker.cancel();
        self.shortcut_capture = None;
        self.shortcut_capture_error = None;
        self.settings_tab = tab;
        self.settings_scroll = 0.0;
    }

    fn on_toggle_scheme_enabled(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        let mut now_disabled = false;
        if let Some(enabled) = self.schemes.iter().find(|s| s.id == id).map(|s| s.enabled) {
            now_disabled = enabled;
            self.edit(doc, scheme_paths(id).enabled.set(!enabled));
        }
        if now_disabled && self.settings.scheme_id == id {
            self.bump_setting(doc, |st| st.scheme_id = String::new());
        }
    }

    fn on_duplicate_scheme(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        let sc = &self.schemes;
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
            self.edit(
                doc,
                Ebook::paths().schemes.insert_as(
                    &ui::tilo_resource::Id::from_raw(new_id.clone()),
                    pos,
                    &dup,
                ),
            );
            self.editing_scheme = Some(new_id);
        }
    }

    fn on_delete_scheme(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        if self.schemes.iter().any(|s| s.id == id && !s.builtin) {
            // The library forgets a deleted scheme the reading settings chose.
            self.edit(
                doc,
                Ebook::DELETE_SCHEME.with(books::DeleteScheme {
                    scheme: id.to_owned(),
                }),
            );
        }
        if self.editing_scheme.as_deref() == Some(id) {
            self.editing_scheme = None;
        }
        if self.settings.scheme_id == id {
            self.settings.scheme_id.clear();
        }
    }

    fn on_edit_scheme(&mut self, id: String, doc: &mut ui::widget::Edits<'_>) {
        self.commit_rename(doc);
        self.editing_scheme = Some(id);
    }

    fn on_close_scheme_editor(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.commit_rename(doc);
        self.editing_scheme = None;
    }

    fn on_new_scheme_via_picker(&mut self, doc: &mut ui::widget::Edits<'_>) {
        // Open the colour picker to choose a background; the scheme is
        // derived + created when the picker is committed.
        self.commit_rename(doc);
        self.color_picker
            .open(PickTarget::NewScheme, [0x1B, 0x18, 0x16]);
    }

    fn on_pick_scheme_color(&mut self, id: String, slot: usize, doc: &mut ui::widget::Edits<'_>) {
        // Edit any scheme colour through the same picker.
        self.commit_rename(doc);
        let cur = self
            .schemes
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.slots().get(slot).map(|&(_, c)| c))
            .unwrap_or([0x80, 0x80, 0x80]);
        self.color_picker.open(PickTarget::Slot(id, slot), cur);
    }

    fn on_begin_rename_scheme(&mut self, id: String, doc: &mut ui::widget::Edits<'_>) {
        self.commit_rename(doc);
        let name = self
            .schemes
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let mut field = EditableLabel::new(&name).with_text_role(TextRole::Body);
        field.begin_edit();
        self.renaming = Some(Rename {
            target: RenameTarget::Scheme(id),
            field,
        });
    }

    fn on_set_default_font(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        self.edit(
            doc,
            Ebook::paths()
                .default_font
                .set(ui::tilo_resource::Atom(id.to_owned())),
        );
        self.font_dropdown.close();
    }

    fn on_toggle_font_shown(&self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        if let Some(shown) = self
            .fonts
            .iter()
            .find(|entry| entry.id == id)
            .map(|f| f.shown)
        {
            self.edit(doc, font_paths(id).shown.set(!shown));
        }
    }

    fn on_remove_font(&mut self, id: &str, doc: &mut ui::widget::Edits<'_>) {
        // The library forgets a removed font the settings chose.
        self.edit(
            doc,
            Ebook::REMOVE_FONT.with(books::RemoveFont {
                font: id.to_owned(),
            }),
        );
        if self.settings.font_id == id {
            self.settings.font_id.clear();
        }
        if self.default_font_id == id {
            self.default_font_id.clear();
        }
    }

    fn on_open_font_picker(&mut self) {
        self.font_picker = true;
        self.picker_scroll = 0.0;
    }

    fn on_add_system_font(&mut self, family: &str, doc: &mut ui::widget::Edits<'_>) {
        let f = self.fonts.clone();
        let dup = f
            .iter()
            .any(|e| matches!(&e.kind, FontKind::Named(n) if n == family));
        if !dup {
            let ids: Vec<String> = f.iter().map(|e| e.id.clone()).collect();
            let id = Self::fresh_id("font", &ids);
            let font = FontEntry::new(&id, FontKind::Named(family.to_string()), family, 1.0, true);
            self.edit(
                doc,
                Ebook::paths().fonts.insert_as(
                    &ui::tilo_resource::Id::from_raw(id),
                    f.len(),
                    &font,
                ),
            );
        }
        self.font_picker = false;
    }

    fn on_begin_rename_font(&mut self, id: String, doc: &mut ui::widget::Edits<'_>) {
        self.commit_rename(doc);
        let entry = self.fonts.iter().find(|e| e.id == id);
        let name = entry.map(|e| e.name.clone()).unwrap_or_default();
        // Render the rename field in the font's own face, so the name
        // is typeset in the typeface it selects while you edit it.
        let fam = entry.map_or(FontChoice::SansSerif, |e| font_choice(&e.kind));
        let mut field = EditableLabel::new(&name)
            .with_text_role(TextRole::Body)
            .with_font_family(fam);
        field.begin_edit();
        self.renaming = Some(Rename {
            target: RenameTarget::Font(id),
            field,
        });
    }

    fn on_set_sort(&mut self, sort: crate::model::LibrarySort, doc: &mut ui::widget::Edits<'_>) {
        self.library_prefs.sort = sort;
        self.edit(doc, Ebook::paths().prefs.sort.set(sort));
        self.shelf_controls.close_sort_menu();
    }

    fn on_toggle_hide_read(&mut self, doc: &mut ui::widget::Edits<'_>) {
        self.library_prefs.hide_read = !self.library_prefs.hide_read;
        self.edit(
            doc,
            Ebook::paths()
                .prefs
                .hide_read
                .set(self.library_prefs.hide_read),
        );
    }

    fn dispatch_shelf_control(
        &mut self,
        action: ShelfControlAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match action {
            ShelfControlAction::ToggleHideRead => self.on_toggle_hide_read(doc),
        }
    }

    fn dispatch_comment_action(&mut self, action: CommentAction, doc: &mut ui::widget::Edits<'_>) {
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

    /// Apply a confirmed pick: create the new scheme (for a from-background
    /// pick) or write the slot edit. Nothing is written while the picker is
    /// open, so *Cancel* leaves the scheme exactly as it was.
    fn commit_picker(&mut self, doc: &mut ui::widget::Edits<'_>, edit: ColorEdit) {
        match edit.target {
            PickTarget::NewScheme => {
                let sc = self.schemes.clone();
                let ids: Vec<String> = sc.iter().map(|s| s.id.clone()).collect();
                let new_id = Self::fresh_id("scheme", &ids);
                let n = sc.iter().filter(|s| !s.builtin).count() + 1;
                let scheme = ColorScheme::from_bg(&new_id, &format!("Scheme {n}"), edit.rgb);
                self.edit(
                    doc,
                    Ebook::paths().schemes.insert_as(
                        &ui::tilo_resource::Id::from_raw(new_id.clone()),
                        sc.len(),
                        &scheme,
                    ),
                );
                self.editing_scheme = Some(new_id.clone());
                self.bump_setting(doc, |st| st.scheme_id = new_id);
            }
            PickTarget::Slot(id, slot) => {
                if let Some(before) = self
                    .schemes
                    .iter()
                    .find(|s| s.id == id && !s.builtin)
                    .cloned()
                {
                    let mut after = before.clone();
                    after.set_slot(slot, edit.rgb);
                    self.edit_scheme(doc, &before, &after);
                }
            }
        }
    }

    /// Force an in-progress inline rename to commit, then persist the
    /// result. Called before an action that would replace or dismiss the
    /// rename. No-op when nothing is being renamed.
    fn commit_rename(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(rn) = self.renaming.as_mut() {
            rn.field.commit();
        }
        self.poll_rename_outcome(doc);
    }

    /// Drain the rename field's latest outcome: on commit, persist the new
    /// name; on commit or cancel, clear the rename. The single place a
    /// rename result is written, so Enter/blur/click-outside all funnel here.
    fn poll_rename_outcome(&mut self, doc: &mut ui::widget::Edits<'_>) {
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
    fn apply_rename(&self, target: RenameTarget, name: &str, doc: &mut ui::widget::Edits<'_>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match target {
            RenameTarget::Font(id) => {
                if self.fonts.iter().any(|e| e.id == id) {
                    self.edit(doc, font_paths(&id).name.replace(name));
                }
            }
            RenameTarget::Scheme(id) => {
                if self.schemes.iter().any(|s| s.id == id) {
                    self.edit(doc, scheme_paths(&id).name.replace(name));
                }
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
            .or_else(|| default_fonts().first().cloned())
            .unwrap_or_else(|| FontEntry::new("default", FontKind::Default, "Default", 1.0, true))
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

    /// Resolve the reader's page palette: a selected, enabled custom scheme wins;
    /// otherwise the built-in tint. Custom schemes carry their own accent.
    /// An OS high-contrast theme still reaches the page — sepia and custom
    /// paper must not ignore it.
    fn resolve_palette(&self, theme: &Theme) -> ReaderPalette {
        if theme.name == Theme::high_contrast().name {
            return ReaderPalette::resolve(ReadTheme::HighContrast, theme);
        }
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

    fn bump_setting(&mut self, doc: &mut ui::widget::Edits<'_>, f: impl FnOnce(&mut Settings)) {
        let before = self.settings.clone();
        let mut s = before.clone();
        f(&mut s);
        for edit in Ebook::paths().settings.path().edits(&before, &s) {
            self.edit(doc, edit);
        }
        self.settings = s;
    }

    /// Change the scheme `before` to `after`, field by field.
    fn edit_scheme(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        before: &ColorScheme,
        after: &ColorScheme,
    ) {
        for edit in scheme_paths(&before.id).path().edits(before, after) {
            self.edit(doc, edit);
        }
    }

    /// Change the font `before` to `after`, field by field.
    fn edit_font(&self, doc: &mut ui::widget::Edits<'_>, before: &FontEntry, after: &FontEntry) {
        for edit in font_paths(&before.id).path().edits(before, after) {
            self.edit(doc, edit);
        }
    }

    /// The ids of the library's annotations, as this window last read them.
    fn annotation_ids(&self) -> std::collections::HashSet<Uuid> {
        self.record.annotations.values().map(|a| a.id).collect()
    }

    /// The annotation this window's last edit added to `doc`: one whose id
    /// is not among `before`.
    fn added_annotation(
        &mut self,
        doc: &AutoCommit,
        before: &std::collections::HashSet<Uuid>,
    ) -> Option<Uuid> {
        self.refresh_from_doc(doc);
        self.record
            .annotations
            .values()
            .map(|a| a.id)
            .find(|id| !before.contains(id))
    }

    /// Resolve a highlight-the-selection gesture into the library's
    /// [`highlight`](Ebook::HIGHLIGHT) verb's parameters. Colours never
    /// layer: applying a colour marks the range, the same colour again
    /// clears it, a different colour replaces it — any highlight the
    /// selection overlaps is dropped first. Returns `None` when there is no
    /// laid-out page, or the gesture resolves to nothing.
    fn resolve_highlight_plan(&self, sel: TextSel, color: HiColor) -> Option<books::Highlight> {
        let (start, end) = sel.ordered();
        // Read from the book, not the layout: a select-passage View publishes
        // highlight-selection before the worker has set the chapter.
        let chapter = self.open_book()?.chapters.get(self.chapter)?;
        let blocks: Vec<(usize, usize, String)> = chapter
            .blocks
            .iter()
            .enumerate()
            .skip(start.block)
            .take((end.block + 1).saturating_sub(start.block))
            .map(|(index, block)| {
                let (text, _) = crate::paginate::selectable_text(block);
                (index, text.chars().count(), text)
            })
            .collect();
        let clens: HashMap<usize, usize> = blocks
            .iter()
            .map(|(index, char_len, _)| (*index, *char_len))
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
        let remove_groups: Vec<Uuid> = overlapping.iter().map(|(g, _)| *g).collect();

        let mut pieces = Vec::new();
        if !toggling_off {
            // One ranged highlight per block the selection touches, each covering
            // exactly the selected characters — so a drag across paragraphs marks
            // precisely what was selected.
            for (index, char_len, text) in &blocks {
                if *index < start.block || *index > end.block {
                    continue;
                }
                let lo = if *index == start.block { start.ch } else { 0 };
                let hi = if *index == end.block {
                    end.ch
                } else {
                    *char_len
                };
                let lo = lo.min(*char_len);
                let hi = hi.min(*char_len);
                if lo >= hi {
                    continue;
                }
                let excerpt: String = text.chars().skip(lo).take((hi - lo).min(120)).collect();
                pieces.push(HiPiece {
                    block: *index,
                    start: lo,
                    end: hi,
                    excerpt,
                });
            }
        }
        if pieces.is_empty() && remove_groups.is_empty() {
            return None;
        }
        Some(crate::actions::highlight_params(
            &self.book_id(),
            self.chapter,
            color,
            remove_groups,
            pieces,
        ))
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
    fn reanchor_highlight(
        &self,
        group: Uuid,
        drop: Point,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
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
        // Comments are also passage destinations in the notes drawer and
        // search. Move their anchors with the highlight, keeping their text
        // and identity rather than leaving links to the old paragraph.
        for annotation in self.annotations.iter().filter(|annotation| {
            annotation.group == group
                && (annotation.id == piece.id || annotation.kind == AnnoKind::Note)
        }) {
            let at = Ebook::paths().annotations.item(&annotation.key());
            self.edit(doc, at.chapter.set(self.chapter));
            self.edit(doc, at.block.set(tb.index));
            self.edit(doc, at.start.set(0));
            self.edit(doc, at.end.set(span));
            self.edit(doc, at.excerpt.replace(excerpt.clone()));
        }
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

    /// Apply one of the library's field edits or verbs now, through its
    /// dispatcher: the one door every control, the palette and agents edit
    /// the library through. A refused edit changes nothing, and the
    /// dispatcher says why.
    fn edit(&self, doc: &mut ui::widget::Edits<'_>, submit: impl ui::framework::Submit<Ebook>) {
        if self.dispatch.submit_now(doc, submit).is_ok() {
            self.record_stale.set(true);
        }
    }

    /// The library's edits bound to where the reader is in the open book.
    pub(crate) fn reading_declarations(
        &self,
    ) -> Vec<ui::tilo_resource::ActionDecl<crate::resource::Ebook>> {
        self.current_book_resource_context()
            .map(|ctx| crate::actions::reading_declarations(&ctx))
            .unwrap_or_default()
    }

    /// Show the shelf settings, put books the library newly lists on the
    /// shelf, and drop books it no longer lists. An empty projection does
    /// not wipe a shelf that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<Ebook>) {
        let previous = std::mem::replace(&mut self.record, Rc::clone(state));
        self.record_stale.set(false);
        self.adopt_shelf(&previous);
        self.refresh_from_record();
    }

    /// Put the books the library newly lists on the shelf and take off the
    /// ones it no longer lists. An empty library does not wipe a shelf that
    /// was never in it.
    fn adopt_shelf(&mut self, previous: &Ebook) {
        let state = Rc::clone(&self.record);
        let left: Vec<String> = previous
            .entries
            .values()
            .filter(|entry| state.entry(&entry.id).is_none())
            .map(|entry| entry.id.clone())
            .collect();
        if state.entries.is_empty() && left.is_empty() {
            return;
        }
        if self.library_loaded {
            let new: Vec<crate::model::LibraryEntry> = state
                .entries
                .values()
                .filter(|entry| !self.hydrated.contains(&entry.id))
                .cloned()
                .collect();
            if !new.is_empty() {
                self.hydrate(&new, &HashMap::new());
            }
        }
        let keep: std::collections::HashSet<&str> = state
            .entries
            .values()
            .map(|entry| entry.id.as_str())
            .collect();
        let gone = |id: &str| {
            if state.entries.is_empty() {
                left.iter().any(|left| left == id)
            } else {
                !keep.contains(id)
            }
        };
        self.shelving.retain(|id| !keep.contains(id.as_str()));
        let shelving = &self.shelving;
        self.hydrated
            .retain(|id| !gone(id) || shelving.contains(id));
        let before = self.library.len();
        std::sync::Arc::make_mut(&mut self.library)
            .retain(|book| !gone(&book.id) || shelving.contains(&book.id));
        if self.library.len() == before {
            return;
        }
        self.library_paths.retain(|id, _| !gone(id));
        self.watched_sources.retain(|_, id| !gone(id));
        self.book_idx = self.book_idx.min(self.library.len().saturating_sub(1));
        if self.library.is_empty() {
            self.view = View::Library;
        }
        self.retire_layout();
    }

    /// Read what the host gives the reader: whether the window is live, the
    /// content store, the sample shelf of a screenshot, and this device's
    /// file grants and watched folders.
    pub(crate) fn attach_host(&mut self, cx: &ui::framework::SurfaceCx<crate::resource::Ebook>) {
        self.live = cx.mode().is_live();
        self.content = cx.content();
        self.sample = cx.fixture::<crate::scenarios::SampleShelf>();
        self.catalogue_setting = Some(cx.settings());
        if self.live {
            let catalogue = crate::catalogue::DeviceCatalogue::on_this_device();
            self.query = query_view::BookQuery::live(cx.waker(), std::sync::Arc::clone(&catalogue));
            self.catalogue = Some(catalogue);
            self.follow_catalogue_setting();
            self.local_library = LocalLibrary::load(cx.storage().settings());
            self.reader_keys = crate::reader_keys::ReaderKeys::following(cx.settings());
            self.watched_folders
                .clone_from(&self.local_library.watched_folders);
            self.scan_watched_folders(&cx.jobs());
        }
    }

    /// Look through the watched folders off the UI thread, and put the books
    /// found there that the shelf does not hold yet on it, through the
    /// library's actions like an import. Their files are this device's
    /// grants, which open them.
    fn scan_watched_folders(&self, jobs: &ui::framework::Jobs<crate::resource::Ebook>) {
        if self.watched_folders.is_empty() {
            return;
        }
        let folders = self.watched_folders.clone();
        jobs.start(
            "watched-folders",
            move |cx: &mut ui::framework::EffectCx<crate::resource::Ebook, Self>| {
                Ok(cx.thread(
                    move |_| content::find_watched_books(&folders),
                    |app: &mut Self, found: Vec<content::FoundBook>, cx| {
                        app.shelve_found(found, cx);
                        Ok(ui::framework::Effected::done())
                    },
                ))
            },
        );
    }

    /// Put `found` books the shelf does not hold yet on it, remembering
    /// where each one's file is.
    fn shelve_found<H: 'static>(
        &mut self,
        found: Vec<content::FoundBook>,
        cx: &ui::framework::EffectCx<crate::resource::Ebook, H>,
    ) {
        let shelved: std::collections::HashSet<String> = cx.live().with(|state| {
            state
                .entries
                .values()
                .map(|entry| entry.id.clone())
                .collect()
        });
        for book in found {
            self.local_library.remember_book(&book.entry.id, &book.path);
            if !shelved.contains(&book.entry.id) {
                cx.submit(crate::resource::shelve_found(&book.entry));
            }
        }
        self.local_library.save();
        self.dirty = true;
    }

    /// Test-only: plant a non-empty text selection so hosted reading extras
    /// bind highlight / note-selection rows.
    #[cfg(test)]
    pub(crate) fn select_passage_for_test(
        &mut self,
        start_block: usize,
        start_ch: usize,
        end_block: usize,
        end_ch: usize,
    ) {
        self.sel = Some(TextSel {
            anchor: DocPos {
                block: start_block,
                ch: start_ch,
            },
            head: DocPos {
                block: end_block,
                ch: end_ch,
            },
        });
        self.selected_block = Some(start_block);
    }

    /// The plain text covered by the current selection, across blocks (truncated
    /// for the annotation excerpt).
    fn selection_excerpt(&self) -> String {
        self.selection_text().chars().take(160).collect()
    }

    /// The full plain text currently selected, paragraph-joined with a
    /// space, not truncated (unlike [`Self::selection_excerpt`]).
    fn selection_text(&self) -> String {
        self.selected_text_joined(false)
    }

    /// The selected text as it goes on the clipboard: verse lines and
    /// `<br/>`s stay on their own lines, and blocks are separated by a blank
    /// line.
    fn selection_clipboard_text(&self) -> String {
        self.selected_text_joined(true)
    }

    fn selected_text_joined(&self, keep_lines: bool) -> String {
        let Some(sel) = self.sel else {
            return String::new();
        };
        let (start, end) = sel.ordered();
        // The book holds the text, not the layout: a selection made by an
        // action reads the same before the worker has set its chapter.
        let Some(chapter) = self
            .open_book()
            .and_then(|book| book.chapters.get(self.chapter))
        else {
            return String::new();
        };
        let mut out = String::new();
        let selected = chapter
            .blocks
            .iter()
            .enumerate()
            .skip(start.block)
            .take((end.block + 1).saturating_sub(start.block));
        for (index, block) in selected {
            let (text, hard_breaks) = crate::paginate::selectable_text(block);
            let char_len = text.chars().count();
            let lo = if index == start.block { start.ch } else { 0 };
            let hi = if index == end.block { end.ch } else { char_len };
            let lo = lo.min(char_len);
            let hi = hi.min(char_len);
            if lo < hi {
                if !out.is_empty() {
                    out.push_str(if keep_lines { "\n\n" } else { " " });
                }
                out.extend(text.chars().enumerate().skip(lo).take(hi - lo).map(
                    |(index, character)| {
                        if keep_lines && hard_breaks.binary_search(&index).is_ok() {
                            '\n'
                        } else {
                            character
                        }
                    },
                ));
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

    /// Logical line span for triple-click. Ebook flow blocks are paragraphs
    /// and headings, so the complete canonical block is the stable line unit
    /// even when presentation reflows it across multiple visual rows.
    fn line_span_at(&self, p: DocPos) -> (DocPos, DocPos) {
        (
            DocPos {
                block: p.block,
                ch: 0,
            },
            DocPos {
                block: p.block,
                ch: self.block_char_len(p.block),
            },
        )
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

    fn toggle_bookmark(&self, doc: &mut ui::widget::Edits<'_>) {
        let block = self.page_top_block;
        self.edit(
            doc,
            Ebook::TOGGLE_BOOKMARK.with(self.bookmark_params(block)),
        );
    }

    /// Bookmarking `block` of the open chapter, or removing its bookmark.
    fn bookmark_params(&self, block: usize) -> books::ToggleBookmark {
        books::ToggleBookmark {
            book: self.book_id(),
            chapter: self.chapter,
            block,
            excerpt: self.block_excerpt(block),
        }
    }

    /// Close an editor whose note or comment a collaborator deleted.
    fn close_orphaned_editors(&mut self) {
        let live = |id: Uuid| {
            self.annotations
                .iter()
                .any(|annotation| annotation.id == id)
        };
        let composing_gone = self.note_composer.id().is_some_and(|id| !live(id));
        let comment_gone = self
            .comment_overlay
            .editing_id()
            .is_some_and(|id| !live(id));
        if composing_gone {
            self.composing = None;
            self.note_composer.close();
        }
        if comment_gone {
            self.comment_overlay.finish_editing();
        }
    }

    fn finish_composing(&mut self, doc: &mut ui::widget::Edits<'_>) -> bool {
        let Some(id) = self.composing.take() else {
            return false;
        };
        let note_editor_open = self.note_composer.is_open();
        if note_editor_open {
            self.note_composer.close();
        }
        let Some(created) = self
            .annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .map(|annotation| annotation.created)
        else {
            return false;
        };
        self.edit(
            doc,
            Ebook::paths()
                .annotations
                .item(&crate::model::annotation_key(id))
                .created
                .set(created.max(unix_now_ms())),
        );
        true
    }

    // --- Hit-test helpers ----------------------------------------------------

    fn push_hit(&mut self, rect: Rect, action: Action) {
        self.hits.push(Hit { rect, action });
    }

    fn is_hover(&self, rect: Rect) -> bool {
        rect.contains(self.cursor)
    }

    /// Stable identity for a painted affordance. Model identities are used in
    /// preference to display positions so an AT-held reference remains valid
    /// when the shelf is sorted or annotations are reordered.
    fn semantic_target(&self, action: &Action) -> Option<String> {
        let target = match action {
            Action::OpenBook(index) => format!("book.open:{}", self.shelf_book(*index)?.id),
            Action::ShareToWorkspace(id) => format!("book.share:{id}"),
            Action::RemoveFromLibrary(id) => format!("book.remove:{id}"),
            Action::Back => "reader.back".into(),
            Action::PrevPage => "reader.previous-page".into(),
            Action::NextPage => "reader.next-page".into(),
            Action::SeekProgress => "reader.progress".into(),
            Action::GotoChapter(index) => format!("reader.chapter:{index}"),
            Action::ToggleToc => "reader.contents".into(),
            Action::ToggleNotes => "reader.annotations".into(),
            Action::ToggleTypePopover => "reader.typography".into(),
            Action::ToggleBookmark => "reader.bookmark".into(),
            Action::FontInc => "reader.font-size.increment".into(),
            Action::FontDec => "reader.font-size.decrement".into(),
            Action::SpacingInc => "reader.line-spacing.increment".into(),
            Action::SpacingDec => "reader.line-spacing.decrement".into(),
            Action::CycleFont => "reader.font.next".into(),
            Action::SetFont(id) => format!("reader.font:{id}"),
            Action::SetReadTheme(theme) => format!("reader.theme:{}", theme.as_str()),
            Action::SetMode(mode) => format!("reader.mode:{}", mode.as_str()),
            Action::SetComposition(style) => format!("reader.composition:{}", style.as_str()),
            Action::SetColor(color) => format!("reader.highlight:{color:?}"),
            Action::AddNote => "reader.add-note".into(),
            Action::CopySelection => "reader.copy-selection".into(),
            Action::JumpTo(id) => format!("annotation.reveal:{id}"),
            Action::Delete(id) => format!("annotation.delete:{id}"),
            Action::EditNote(id) => format!("annotation.edit:{id}"),
            Action::OpenComments(id) => format!("highlight.comments:{id}"),
            Action::CloseComments => "highlight.comments.close".into(),
            Action::DeleteHighlight(id) => format!("highlight.delete:{id}"),
            Action::AddComment(id) => format!("highlight.comment.add:{id}"),
            Action::EditComment(id) => format!("highlight.comment.edit:{id}"),
            Action::DeleteComment(id) => format!("highlight.comment.delete:{id}"),
            Action::OpenViaPortal => "library.open".into(),
            Action::ToggleWatchFolder(path) => {
                format!("library.watch:{}", ui::a11y::hash_key(path))
            }
            Action::ToggleKeepCatalogues => "library.keep-catalogues".into(),
            Action::OpenSettings => "settings.open".into(),
            Action::CloseSettings => "settings.close".into(),
            Action::SetSettingsTab(tab) => format!("settings.tab:{tab:?}"),
            Action::BeginShortcutCapture(shortcut) => {
                format!("settings.shortcut:{}", shortcut.id())
            }
            Action::SetScheme(id) => format!("settings.scheme.select:{id}"),
            Action::ToggleSchemeEnabled(id) => format!("settings.scheme.enabled:{id}"),
            Action::DuplicateScheme(id) => format!("settings.scheme.duplicate:{id}"),
            Action::DeleteScheme(id) => format!("settings.scheme.delete:{id}"),
            Action::EditScheme(id) => format!("settings.scheme.edit:{id}"),
            Action::CloseSchemeEditor => "settings.scheme-editor.close".into(),
            Action::NewSchemeViaPicker => "settings.scheme.new".into(),
            Action::PickSchemeColor(id, slot) => format!("settings.scheme.color:{id}:{slot}"),
            Action::BeginRenameScheme(id) => format!("settings.scheme.rename:{id}"),
            Action::ToggleFontShown(id) => format!("settings.font.shown:{id}"),
            Action::RemoveFont(id) => format!("settings.font.remove:{id}"),
            Action::OpenFontPicker => "settings.font.add".into(),
            Action::CloseFontPicker => "settings.font-picker.close".into(),
            Action::AddSystemFont(family) => {
                format!("settings.font.add:{}", ui::a11y::hash_key(family))
            }
            Action::BeginRenameFont(id) => format!("settings.font.rename:{id}"),
            Action::OpenHit(anchor) => format!("search.open:{anchor}"),
            Action::GetBook(id) => format!("search.get:{id}"),
            Action::CloseSearch => "search.back".into(),
            Action::DismissOverlay
            | Action::Swallow
            | Action::SetQuery(_)
            | Action::OpenFacet(_) => return None,
        };
        Some(format!("books.{target}"))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive action-to-accessible-label mapping is intentionally co-located"
    )]
    fn semantic_label(&self, action: &Action) -> String {
        match action {
            Action::OpenBook(index) => self
                .shelf_book(*index)
                .map_or_else(|| "Open book".into(), |book| format!("Open {}", book.title)),
            Action::ShareToWorkspace(id) => {
                self.library.iter().find(|book| book.id == *id).map_or_else(
                    || "Share book with workspace".into(),
                    |book| format!("Share {} with workspace", book.title),
                )
            }
            Action::RemoveFromLibrary(id) => {
                self.library.iter().find(|book| book.id == *id).map_or_else(
                    || "Remove book from library".into(),
                    |book| format!("Remove {} from library", book.title),
                )
            }
            Action::Back | Action::CloseSettings => "Back to library".into(),
            Action::PrevPage => "Previous page".into(),
            Action::NextPage => "Next page".into(),
            Action::SeekProgress => {
                format!(
                    "Reading progress: {}",
                    progress_label(self.reading_progress)
                )
            }
            Action::GotoChapter(index) => self
                .open_book()
                .and_then(|book| book.chapters.get(*index))
                .map_or_else(
                    || format!("Go to chapter {}", index + 1),
                    |c| c.title.clone(),
                ),
            Action::ToggleToc => "Contents".into(),
            Action::ToggleNotes => "Annotations".into(),
            Action::ToggleTypePopover => "Reading appearance".into(),
            Action::ToggleBookmark => "Bookmark this page".into(),
            Action::FontInc => "Increase text size".into(),
            Action::FontDec => "Decrease text size".into(),
            Action::SpacingInc => "Increase line spacing".into(),
            Action::SpacingDec => "Decrease line spacing".into(),
            Action::CycleFont => "Use next typeface".into(),
            Action::SetFont(id) => self
                .fonts
                .iter()
                .find(|font| font.id == *id)
                .map_or_else(|| id.clone(), |font| font.name.clone()),
            Action::SetReadTheme(theme) => format!("{} reading theme", theme.label()),
            Action::SetMode(mode) => format!("{} layout", mode.label()),
            Action::SetComposition(style) => format!("{} text", style.label()),
            Action::SetColor(color) => format!("Highlight {color:?}").to_lowercase(),
            Action::AddNote => "Add note".into(),
            Action::CopySelection => "Copy selected text".into(),
            Action::JumpTo(id) => self.annotation_label(*id, "Reveal annotation"),
            Action::Delete(id) => self.annotation_label(*id, "Delete annotation"),
            Action::EditNote(id) => self.annotation_label(*id, "Edit note"),
            Action::OpenComments(_) => "Open highlight comments".into(),
            Action::CloseComments => "Close comments".into(),
            Action::DeleteHighlight(_) => "Delete highlight".into(),
            Action::AddComment(_) => "Add comment".into(),
            Action::EditComment(id) => self.annotation_label(*id, "Edit comment"),
            Action::DeleteComment(id) => self.annotation_label(*id, "Delete comment"),
            Action::OpenViaPortal => "Open book".into(),
            Action::ToggleWatchFolder(path) => format!("Watch {}", path.display()),
            Action::ToggleKeepCatalogues => crate::catalogue::KEEP_CATALOGUES.into(),
            Action::OpenSettings => "Book settings".into(),
            Action::SetSettingsTab(SettingsTab::Fonts) => "Fonts".into(),
            Action::SetSettingsTab(SettingsTab::Colours) => "Colour themes".into(),
            Action::SetSettingsTab(SettingsTab::Library) => "Library folders".into(),
            Action::SetSettingsTab(SettingsTab::Shortcuts) => "Keyboard shortcuts".into(),
            Action::BeginShortcutCapture(shortcut) => {
                if self.shortcut_capture == Some(*shortcut) {
                    self.shortcut_capture_error.clone().map_or_else(
                        || format!("Press a key for {}; Escape cancels", shortcut.label()),
                        |error| format!("{}: {error}", shortcut.label()),
                    )
                } else {
                    format!(
                        "Change {} shortcut, currently {}",
                        shortcut.label(),
                        self.reader_keys
                            .shortcut(shortcut.id(), shortcut.default_key())
                    )
                }
            }
            Action::SetScheme(id) => self.scheme_name(id),
            Action::ToggleSchemeEnabled(id) => format!("Show {} in reader", self.scheme_name(id)),
            Action::DuplicateScheme(id) => format!("Duplicate {}", self.scheme_name(id)),
            Action::DeleteScheme(id) => format!("Delete {}", self.scheme_name(id)),
            Action::EditScheme(id) => format!("Edit {}", self.scheme_name(id)),
            Action::CloseSchemeEditor => "Done editing theme".into(),
            Action::NewSchemeViaPicker => "New colour theme".into(),
            Action::PickSchemeColor(id, slot) => format!(
                "Edit {} {} colour",
                self.scheme_name(id),
                ["page", "text", "accent"]
                    .get(*slot)
                    .copied()
                    .unwrap_or("custom")
            ),
            Action::BeginRenameScheme(id) => format!("Rename {}", self.scheme_name(id)),
            Action::ToggleFontShown(id) => format!("Show {} in reader", self.font_name(id)),
            Action::RemoveFont(id) => format!("Remove {}", self.font_name(id)),
            Action::OpenFontPicker => "Add font".into(),
            Action::CloseFontPicker => "Close font picker".into(),
            Action::AddSystemFont(family) => format!("Add {family}"),
            Action::BeginRenameFont(id) => format!("Rename {}", self.font_name(id)),
            Action::OpenHit(anchor) => self
                .query
                .label_of(anchor)
                .map_or_else(|| "Open result".into(), str::to_owned),
            Action::GetBook(id) => self
                .query
                .label_of(&format!("gutenberg:{id}"))
                .map_or_else(|| "Get book".into(), str::to_owned),
            Action::CloseSearch => "Back".into(),
            Action::DismissOverlay
            | Action::Swallow
            | Action::SetQuery(_)
            | Action::OpenFacet(_) => String::new(),
        }
    }

    /// Refresh the hover description from the topmost live hit. The reader
    /// draws several icon-only affordances on a narrow viewport, and a hit can
    /// also be covered by a modal; using the final hit stack means the tooltip
    /// always names the action that would actually receive a click.
    fn refresh_hover_tooltip(&mut self) {
        self.hover_tooltip = self
            .hits
            .iter()
            .rev()
            .find(|hit| hit.rect.contains(self.cursor))
            .map(|hit| self.semantic_label(&hit.action))
            .filter(|label| !label.is_empty());
    }

    fn annotation_label(&self, id: Uuid, fallback: &str) -> String {
        self.all_annotations
            .iter()
            .find(|annotation| annotation.id == id)
            .map_or_else(
                || fallback.into(),
                |annotation| {
                    let note = annotation.note.clone();
                    let text = if note.trim().is_empty() {
                        annotation.excerpt.trim()
                    } else {
                        note.trim()
                    };
                    if text.is_empty() {
                        fallback.into()
                    } else {
                        format!("{fallback}: {text}")
                    }
                },
            )
    }

    fn font_name(&self, id: &str) -> String {
        self.fonts
            .iter()
            .find(|font| font.id == id)
            .map_or_else(|| id.to_string(), |font| font.name.clone())
    }

    fn scheme_name(&self, id: &str) -> String {
        self.schemes
            .iter()
            .find(|scheme| scheme.id == id)
            .map_or_else(|| id.to_string(), |scheme| scheme.name.clone())
    }

    fn semantic_role_state(&self, action: &Action) -> (Role, AccessState) {
        let focused = self
            .semantic_target(action)
            .is_some_and(|target| self.semantic_focus.as_deref() == Some(&target));
        let base = AccessState::new().focusable(true).focused(focused);
        match action {
            Action::SeekProgress => (Role::Slider, base),
            Action::ToggleToc => (Role::ToggleButton, base.checked(self.toc_open())),
            Action::ToggleNotes => (Role::ToggleButton, base.checked(self.notes_open())),
            Action::ToggleTypePopover => (
                Role::ToggleButton,
                base.checked(self.reading_settings.is_open()),
            ),
            Action::ToggleBookmark => (Role::ToggleButton, base.checked(self.page_bookmarked())),
            Action::SetSettingsTab(tab) => {
                (Role::RadioButton, base.checked(self.settings_tab == *tab))
            }
            Action::SetReadTheme(theme) => (
                Role::RadioButton,
                base.checked(self.settings.theme == *theme && self.settings.scheme_id.is_empty()),
            ),
            Action::SetMode(mode) => (Role::RadioButton, base.checked(self.settings.mode == *mode)),
            Action::SetComposition(style) => (
                Role::RadioButton,
                base.checked(self.settings.composition == *style),
            ),
            Action::SetFont(id) => (
                Role::RadioButton,
                base.checked(self.settings.font_id == *id),
            ),
            Action::SetScheme(id) => (
                Role::RadioButton,
                base.checked(self.settings.scheme_id == *id),
            ),
            Action::ToggleFontShown(id) => (
                Role::Switch,
                base.checked(
                    self.fonts
                        .iter()
                        .find(|font| font.id == *id)
                        .is_some_and(|f| f.shown),
                ),
            ),
            Action::ToggleSchemeEnabled(id) => (
                Role::Switch,
                base.checked(
                    self.schemes
                        .iter()
                        .find(|scheme| scheme.id == *id)
                        .is_some_and(|s| s.enabled),
                ),
            ),
            Action::ToggleWatchFolder(path) => (
                Role::Checkbox,
                base.checked(self.watched_folders.contains(path)),
            ),
            Action::ToggleKeepCatalogues => (Role::Switch, base.checked(self.keeps_catalogues())),
            _ => (Role::Button, base),
        }
    }

    fn page_bookmarked(&self) -> bool {
        self.annotations.iter().any(|annotation| {
            annotation.kind == AnnoKind::Bookmark
                && annotation.chapter == self.chapter
                && annotation.block == self.page_top_block
        })
    }

    fn action_is_accessibly_visible(&self, action: &Action) -> bool {
        if let Some(id) = &self.editing_scheme {
            return matches!(action, Action::CloseSchemeEditor)
                || matches!(action, Action::BeginRenameScheme(action_id) if action_id == id)
                || matches!(action, Action::PickSchemeColor(action_id, _) if action_id == id);
        }
        if self.font_picker {
            return matches!(action, Action::CloseFontPicker | Action::AddSystemFont(_));
        }
        true
    }

    fn reading_document_node(&self) -> Option<AccessNode> {
        if self.view != View::Reader {
            return None;
        }
        let chapter = self.open_book()?.chapters.get(self.chapter)?;
        let first = self.page_first_doc.map_or(0, |position| position.block);
        let last = self.page_last_doc.map_or_else(
            || chapter.blocks.len().saturating_sub(1),
            |position| position.block,
        );
        let mut page_bounds: Option<Rect> = None;
        let children = chapter
            .blocks
            .iter()
            .enumerate()
            .filter(|(index, block)| *index >= first && *index <= last && !block.is_separator())
            .map(|(index, block)| {
                let role = match block {
                    crate::model::Block::Heading { level, .. } => Role::Heading { level: *level },
                    _ => Role::Paragraph,
                };
                let node = AccessNode::new(role)
                    .with_name(block.plain_text())
                    .with_hashed_key((self.chapter, index));
                match self.painted_block_bounds(index) {
                    Some(bounds) => {
                        page_bounds = Some(page_bounds.map_or(bounds, |page| page.union(bounds)));
                        node.with_bounds(bounds)
                    }
                    None => node,
                }
            })
            .collect();
        let document = AccessNode::new(Role::Document)
            .with_name(chapter.title.clone())
            .with_description("Current reading page")
            .with_state(AccessState::new().read_only(true))
            .with_children(children);
        Some(match page_bounds {
            Some(bounds) => document.with_bounds(bounds),
            None => document,
        })
    }

    /// Screen bounds of the part of block `index` painted in the last frame:
    /// the union of its on-page selection runs. A block continued from or on
    /// another page reports only its visible lines.
    fn painted_block_bounds(&self, index: usize) -> Option<Rect> {
        self.sel_runs
            .iter()
            .filter(|run| run.block == index)
            .map(|run| Rect::new(run.x0, run.y0, run.x1, run.y1))
            .reduce(|bounds, run| bounds.union(run))
    }

    /// An open failed: say so in a toast, and in the empty library for as
    /// long as it stands.
    fn report_failure(&mut self, message: String) {
        self.open_failure = Some(message.clone());
        self.load_error.show(message);
    }

    /// What failed no longer stands: a book opened or another import began.
    fn clear_failure(&mut self) {
        self.open_failure = None;
        self.load_error.dismiss();
    }

    /// Say an import in flight in the window's bar, for as long as it runs
    /// (parsing happens off-thread, so the previous book stays interactive),
    /// or the last import's failure, for a notice's lifetime.
    fn expire_load_toast(&mut self) {
        if let Some(loading) = self.import_loading.as_mut() {
            // An import in flight is the work its notice follows.
            loading.card.keep_while(true);
            loading.card.expire();
            let name = loading
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("book");
            let text = format!("Opening {name}");
            loading
                .card
                .publish(|()| ToolbarNotice::new(text, ToastKind::Info));
        }
        self.load_error.expire();
        self.load_error
            .publish(|error| ToolbarNotice::new(error.clone(), ToastKind::Error));
    }

    /// When the import's card leaves, for the app's wake deadline.
    fn load_toast_deadline(&self) -> Option<ui::time::Instant> {
        self.import_loading
            .as_ref()
            .and_then(|loading| loading.card.deadline())
            .into_iter()
            .chain(self.load_error.deadline())
            .min()
    }
}

/// Where the scheme `id` is edited in the library.
fn scheme_paths(id: &str) -> <ColorScheme as ui::tilo_resource::Model>::Paths<Ebook> {
    Ebook::paths()
        .schemes
        .item(&ui::tilo_resource::Id::from_raw(id))
}

/// Where the font `id` is edited in the library.
fn font_paths(id: &str) -> <FontEntry as ui::tilo_resource::Model>::Paths<Ebook> {
    Ebook::paths()
        .fonts
        .item(&ui::tilo_resource::Id::from_raw(id))
}

impl Continuable for BooksApp {
    fn continuation_metadata(
        &self,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        let book = self.library.get(self.book_idx)?;
        // Book ids are content identities, never source paths. Refuse an
        // unexpected id rather than turning a continuation report into a
        // local filesystem disclosure.
        if book.id.is_empty() || book.id.len() > 256 || book.id.contains(['/', '\\']) {
            return None;
        }
        let bytes = serde_json::to_vec(&serde_json::json!({
            "book_id": book.id,
            "chapter": self.chapter,
            "block": self.reading_block(),
        }))
        .ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: ui::collaboration::semantic_schema::PUBLICATION_POSITION_V1,
            bytes,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        _doc: &mut ui::widget::Edits<'_>,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != ui::collaboration::semantic_schema::PUBLICATION_POSITION_V1 {
            return false;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&metadata.bytes) else {
            return false;
        };
        let Some(book_id) = value.get("book_id").and_then(serde_json::Value::as_str) else {
            return false;
        };
        let Some(index) = self.library.iter().position(|book| book.id == book_id) else {
            return false;
        };
        let last_chapter = self
            .library
            .get(index)
            .map_or(0, |book| book.chapters.len().saturating_sub(1));
        let chapter = value
            .get("chapter")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
            .min(last_chapter);
        let block = value
            .get("block")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0);
        self.book_idx = index;
        self.view = View::Reader;
        self.chapter = chapter;
        self.page = 0;
        self.pending_jump_block = Some(block);
        // The accepted passage is explicit presentation. Ordinary sync must
        // not replace it with the record's older saved reading progress.
        self.adopt_position = false;
        self.retire_layout();
        self.dirty = true;
        true
    }
}

impl Widget for BooksApp {
    ui::provides!(dyn ui::widget::Continuable, dyn ui::widget::Queryable);

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        self.ensure_library_loaded(edits.doc());
        self.drain_import_worker(edits);
        self.commit_pending_share(edits);
        self.open_pending_book(edits);
        self.reveal_arrived();
        self.keep_built_segments(edits);
        self.deliver_effect_replies();
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        let (doc, path) = ui::doc::document_ref(doc, path);
        // Hydrate the persisted library on the first frame (when the document is
        // finally available), then pull in any book the host picker returned.
        self.ensure_library_loaded(doc);
        self.drain_opens();
        self.expire_load_toast();
        self.poll_query();
        if self.absorb_layout_chunks() {
            self.dirty = true;
        }
        self.close_orphaned_editors();
        self.note_composer.sync(doc, path);
        self.comment_overlay.sync(doc, path);
        // Once a book opens, take up its stored position.
        if self.adopt_position && self.view == View::Reader {
            self.adopt_position = false;
            let id = self.book_id();
            let p = self.record.progress_of(&id);
            let nchap = self.num_chapters().saturating_sub(1);
            self.chapter = p.chapter.min(nchap);
            self.page = 0;
            self.pending_jump_block = Some(p.block);
        }
        self.refresh_from_doc(doc);
        let registry: Vec<crate::model::LibraryEntry> =
            self.record.entries.values().cloned().collect();
        self.sync_library_index(&registry);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for book in self.library.iter() {
            book.id.hash(&mut hasher);
        }
        for annotation in self.all_annotations.iter() {
            annotation.id.hash(&mut hasher);
            annotation.doc_id.hash(&mut hasher);
            annotation.chapter.hash(&mut hasher);
            annotation.block.hash(&mut hasher);
            annotation.start.hash(&mut hasher);
            annotation.end.hash(&mut hasher);
            annotation.excerpt.hash(&mut hasher);
            annotation.note.hash(&mut hasher);
            std::mem::discriminant(&annotation.kind).hash(&mut hasher);
            std::mem::discriminant(&annotation.color).hash(&mut hasher);
        }
        for entry in &registry {
            entry.id.hash(&mut hasher);
            entry.title.hash(&mut hasher);
            entry.author.hash(&mut hasher);
            entry.subject.hash(&mut hasher);
            entry.shared.hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        if let Some(build) = self.search_worker.as_ref().and_then(SearchWorker::try_recv) {
            self.search_worker = None;
            if build.fingerprint == fingerprint {
                // Publication is a single UI-thread assignment. Sessions that
                // already hold the prior Arc remain valid until their own
                // generation check rejects the continuation.
                self.book_search = build.provider;
                self.search_fingerprint = fingerprint;
                self.search_revision = build.generation;
            }
        }
        if fingerprint != self.search_fingerprint && self.search_worker.is_none() {
            let Some(generation) = self.search_revision.checked_add(1) else {
                // Never wrap a generation: a stale palette cursor must not
                // become valid again after integer exhaustion.
                self.book_search = crate::search_provider::BookSearch::new_shared(
                    Arc::clone(&self.library),
                    Arc::clone(&self.all_annotations),
                    Arc::new(registry),
                    0,
                );
                return;
            };
            let registry = Arc::new(registry);
            if let Ok(worker) = SearchWorker::spawn(
                self.book_search.clone(),
                Arc::clone(&self.library),
                Arc::clone(&self.all_annotations),
                registry,
                fingerprint,
                generation,
            ) {
                self.search_worker = Some(worker);
            } else {
                // A worker construction failure withdraws the directory;
                // publishing stale canonical resources would be worse.
                self.book_search = crate::search_provider::BookSearch::new_shared(
                    Arc::clone(&self.library),
                    Arc::clone(&self.all_annotations),
                    Arc::new(Vec::new()),
                    0,
                );
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        // Rail reconciliation happens at the start of `draw`, once both the
        // allocated width and the host accessibility text scale are known.
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.hits.clear();
        self.comment_overlay.begin_frame();
        self.number_controls.begin_frame();
        self.reading_settings.begin_frame();
        self.shelf_controls.begin_frame();
        self.font_dropdown.begin_frame();
        self.header_h = f64::from(theme.tokens.metrics.header_height);
        self.reader_spacing = theme.tokens.spacing;
        self.reader_compact_breakpoint = f64::from(theme.tokens.metrics.compact_breakpoint);
        self.selection_hud_h = ui::SelectionHud::height(theme);
        self.bubble_h = ui::status_bubble::pill_height(theme);
        // Resolve the drawers against this frame's width (a breakpoint
        // crossing settles them without a slide), then advance any toggle.
        self.toc_drawer.scale_for(theme);
        self.notes_drawer.scale_for(theme);
        self.resolve_drawers();
        if let Some((toc, notes)) = self.shot_drawers.take() {
            self.toc_drawer.snap(toc);
            self.notes_drawer.snap(notes);
        }
        if theme.reduce_motion || self.reduce_motion {
            self.page_turn = None;
        }
        self.toc_drawer.tick(theme);
        self.notes_drawer.tick(theme);
        self.resolve_drawers();
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        match self.view {
            View::Library => self.draw_library(&mut ctx),
            View::Reader => self.draw_reader(&mut ctx),
            View::Settings => self.draw_settings(&mut ctx),
            View::Search => self.draw_query_view(&mut ctx),
        }
        // The open list of a dropdown lies over everything.
        let window = self.rect;
        self.shelf_controls.draw_overlay(&mut ctx, window);
        if self.query.facets.is_open() {
            self.query.facets.menu_mut().draw_overlay(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                presence,
            );
        }
        self.font_dropdown
            .draw_overlay(ctx.scene, ctx.transform, ctx.text, ctx.theme, window);
        self.refresh_hover_tooltip();
        if let Some(focused) = self.semantic_focus.as_deref()
            && let Some(hit) = self
                .hits
                .iter()
                .rev()
                .find(|hit| self.semantic_target(&hit.action).as_deref() == Some(focused))
        {
            ui::widget::paint_focus_ring(
                scene,
                transform,
                hit.rect,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.route_query_event(event, ctx) {
            return;
        }
        let mut input_ctx = InputEventCtx {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state.reborrow(),
            root: ctx.path,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
            handled: ctx.handled,
        };
        // The colour sheet is modal: while open every event is its own.
        if let Some(response) = self.color_picker.handle_event(&mut input_ctx) {
            if let ColorSheetResponse::Confirmed(edit) = response {
                self.commit_picker(&mut input_ctx.state, edit);
            }
            self.refresh_from_doc(input_ctx.state.doc());
            self.dirty = true;
            return;
        }
        if self.forward_rename_event(&mut input_ctx) {
            return;
        }
        if self.forward_dropdown_event(&mut input_ctx) {
            return;
        }
        if self.forward_shared_input_event(&mut input_ctx) {
            return;
        }

        if let Some(file) = self.file_open.handle_event(event) {
            if let ui::widget::FileOpenEvent::Open(path) = file {
                self.open_dropped(path);
            }
            self.dirty = true;
            *ctx.handled = true;
            return;
        }

        match event {
            // Regaining focus is a natural moment to notice books that appeared
            // in a watched folder while the reader was elsewhere — no manual
            // "reload". A no-op when nothing is watched or nothing is new.
            WindowEvent::Focused(true) => self.rescan_watched(ctx.state.doc()),
            WindowEvent::CursorMoved { position, .. } => {
                let p = ctx.logical(position);
                self.handle_cursor_moved(p, &mut ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press(*ctx.modifiers, &mut ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_release(&mut ctx.state, ctx.handled);
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                if self.handle_key(&ke.logical_key, ctx.modifiers.state(), &mut ctx.state) {
                    self.refresh_from_doc(ctx.state.doc());
                    self.persist_progress(&mut ctx.state);
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(*delta, &mut ctx.state, ctx.handled);
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

    fn tooltip(&self) -> Option<&str> {
        self.hover_tooltip.as_deref()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the retained accessibility tree is intentionally assembled in one place so modal visibility and semantic routing remain auditable"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        let mut seen = std::collections::HashSet::new();
        let mut controls = self
            .hits
            .iter()
            .rev()
            .filter_map(|hit| {
                if !self.action_is_accessibly_visible(&hit.action) {
                    return None;
                }
                let target = self.semantic_target(&hit.action)?;
                if !seen.insert(target.clone()) {
                    return None;
                }
                let (role, state) = self.semantic_role_state(&hit.action);
                let node = AccessNode::new(role)
                    .with_name(self.semantic_label(&hit.action))
                    .with_state(state)
                    .with_bounds(hit.rect)
                    .with_action_target(target);
                // A book's tile carries where its text stands while it is
                // neither here nor on its way.
                let status = match &hit.action {
                    Action::OpenBook(index) => self
                        .shelf_book(*index)
                        .and_then(|book| self.shelf_status(&book.id)),
                    _ => None,
                };
                if let Some((label, detail)) = status {
                    let node = node.with_value(label);
                    return Some(match detail {
                        Some(detail) => node.with_description(detail),
                        None => node,
                    });
                }
                Some(if matches!(hit.action, Action::SeekProgress) {
                    node.with_value(format!(
                        "{}%",
                        (self.reading_progress * 100.0).round() as u8
                    ))
                    .with_description(
                        "Drag or use left and right arrow keys to move through the book",
                    )
                } else {
                    node
                })
            })
            .collect::<Vec<_>>();
        controls.reverse();
        // A modal surface is the whole interactive world while it is open;
        // do not leave the visually dimmed reader/settings controls navigable
        // behind it. This also gives Tab a natural focus trap.
        let modal_controls = if self.color_picker.is_open() {
            self.color_picker.accessibility_nodes()
        } else if self.note_composer.is_open() {
            self.note_composer.accessibility_nodes()
        } else if self.comment_overlay.is_open() {
            self.comment_overlay
                .accessibility_nodes(&self.annotations, self.semantic_focus.as_deref())
        } else if self.reading_settings.is_open() {
            self.reading_settings
                .accessibility_nodes(&self.settings, self.semantic_focus.as_deref())
        } else {
            Vec::new()
        };
        let has_modal = !modal_controls.is_empty();
        if has_modal {
            controls = modal_controls;
        } else {
            if self.view == View::Library {
                controls.extend(
                    self.shelf_controls
                        .accessibility_nodes(self.library_prefs, self.semantic_focus.as_deref()),
                );
            }
            if self.view == View::Settings
                && let Some(node) = self.font_dropdown.accessibility()
            {
                controls.push(node);
            }
            if self.view == View::Search {
                controls.extend(self.query.chips.access_nodes());
            }
            controls.extend(self.number_controls.accessibility_nodes());
            if let Some(rename) = &self.renaming
                && let Some(node) = rename.field.accessibility()
            {
                let target = match &rename.target {
                    RenameTarget::Font(id) => format!("books.rename.font:{id}"),
                    RenameTarget::Scheme(id) => format!("books.rename.scheme:{id}"),
                };
                controls.push(
                    node.with_name("Name")
                        .with_bounds(rename.field.get_rect())
                        .with_action_target(target),
                );
            }
        }

        let (name, description) = match self.view {
            View::Library => ("Books library", "Browse and open books"),
            View::Reader => ("Book reader", "Read and annotate the current book"),
            View::Settings => (
                "Book settings",
                "Manage reading fonts, colours, and library folders",
            ),
            View::Search => ("Search books", "Results in your books and the store"),
        };
        let mut children = Vec::new();
        if !has_modal && let Some(document) = self.reading_document_node() {
            children.push(document);
        }
        if !has_modal && self.view == View::Reader {
            if self.toc_open() {
                children.push(AccessNode::new(Role::StaticText).with_name(format!(
                    "Contents: chapter {} of {}. Scroll this list to browse every chapter.",
                    self.chapter + 1,
                    self.num_chapters()
                )));
            }
            if self.notes_open() {
                children.push(AccessNode::new(Role::StaticText).with_name(format!(
                    "Annotations: {} total. Scroll this list to browse every annotation.",
                    self.annotations.len()
                )));
            }
        }
        children.extend(controls);
        if let Some(error) = self.load_error.get().filter(|error| !error.is_empty()) {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(error.clone())
                    .with_live(ui::a11y::Politeness::Assertive),
            );
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name(name)
                .with_description(description)
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one routing table keeps every synthetic Books control on its existing interaction path"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let Some(path) = path.downcast_ref::<ObjId>() else {
            return false;
        };
        if let Some(id) = target.strip_prefix("books.search.") {
            let performed = self.query.chips.perform(id, action, doc);
            for asked in self.query.chips.take_asked() {
                self.dispatch(asked, doc);
            }
            self.dirty |= performed;
            return performed;
        }
        if target == "books.reader.progress" {
            self.semantic_focus = Some(target.to_string());
            let value = match action {
                A11yAction::Increment => Some(self.reading_progress + 0.05),
                A11yAction::Decrement => Some(self.reading_progress - 0.05),
                A11yAction::SetValue(value) => Self::accessible_progress_value(value),
                A11yAction::Focus | A11yAction::ScrollIntoView => {
                    self.dirty = true;
                    return true;
                }
                _ => None,
            };
            let Some(value) = value else {
                return false;
            };
            self.on_seek_progress(value);
            self.refresh_from_doc(doc);
            self.persist_progress(doc);
            self.dirty = true;
            return true;
        }
        if let (true, picked) = self
            .shelf_controls
            .perform_dropdown_action(target, action, doc, path)
        {
            if let Some(sort) = picked {
                self.on_set_sort(sort, doc);
            }
            self.semantic_focus = Some(target.to_string());
            self.dirty = true;
            return true;
        }
        if self.font_dropdown.perform_action(target, action, doc, path) {
            if let Some(id) = self.font_dropdown.take_change() {
                self.on_set_default_font(&id, doc);
                self.refresh_from_doc(doc);
            }
            self.semantic_focus = Some(target.to_string());
            self.dirty = true;
            return true;
        }
        if let Some(shelf_action) = ShelfControls::semantic_action(target) {
            self.semantic_focus = Some(target.to_string());
            self.dirty = true;
            if matches!(action, A11yAction::Focus | A11yAction::ScrollIntoView) {
                return true;
            }
            if matches!(
                action,
                A11yAction::Activate
                    | A11yAction::ToggleCheck
                    | A11yAction::Expand
                    | A11yAction::Collapse
            ) {
                self.dispatch_shelf_control(shelf_action, doc);
                self.refresh_from_doc(doc);
                return true;
            }
            return false;
        }
        if let Some(settings_action) = self.reading_settings.semantic_action(target) {
            self.semantic_focus = Some(target.to_string());
            self.dirty = true;
            if matches!(action, A11yAction::Focus | A11yAction::ScrollIntoView) {
                return true;
            }
            if matches!(action, A11yAction::Activate | A11yAction::ToggleCheck) {
                self.dispatch_reading_settings(settings_action, doc);
                self.refresh_from_doc(doc);
                self.persist_progress(doc);
                return true;
            }
            return false;
        }
        if let Some(settings_action) = self
            .reading_settings
            .perform_number_semantic_action(target, action, doc, path)
        {
            self.dispatch_reading_settings(settings_action, doc);
            self.refresh_from_doc(doc);
            self.persist_progress(doc);
            self.dirty = true;
            return true;
        }
        if let Some(comment_action) = self.comment_overlay.semantic_action(target) {
            self.semantic_focus = Some(target.to_string());
            self.dirty = true;
            if matches!(action, A11yAction::Focus | A11yAction::ScrollIntoView) {
                return true;
            }
            if matches!(action, A11yAction::Activate) {
                self.dispatch_comment_action(comment_action, doc);
                self.refresh_from_doc(doc);
                return true;
            }
            return false;
        }
        if self
            .comment_overlay
            .perform_editor_semantic_action(target, action, doc, path)
        {
            self.refresh_from_doc(doc);
            self.dirty = true;
            return true;
        }
        if let Some(outcome) = self
            .note_composer
            .perform_semantic_action(target, action, doc, path)
        {
            match outcome {
                NoteSemanticOutcome::Save => {
                    self.finish_composing(doc);
                }
                NoteSemanticOutcome::Delete(id) => self.on_delete_annotation(id, doc),
                NoteSemanticOutcome::Focused => {}
            }
            self.refresh_from_doc(doc);
            self.dirty = true;
            return true;
        }
        if let Some(response) = self
            .color_picker
            .perform_semantic_action(target, action, doc, path)
        {
            if let ColorSheetResponse::Confirmed(edit) = response {
                self.commit_picker(doc, edit);
            }
            self.refresh_from_doc(doc);
            self.dirty = true;
            return true;
        }
        if let Some((field, value)) = self
            .number_controls
            .perform_semantic_action(target, action, doc, path)
        {
            if let Some(value) = value {
                match field {
                    NumberField::FontScale => self.bump_setting(doc, |settings| {
                        settings.font_scale = (value / 100.0) as f32;
                    }),
                    NumberField::LineSpacing => self.bump_setting(doc, |settings| {
                        settings.line_spacing = value as f32;
                    }),
                    NumberField::FontMult(id) => {
                        if let Some(before) = self.fonts.iter().find(|font| font.id == id).cloned()
                        {
                            let after = FontEntry {
                                size_mult: (value / 100.0) as f32,
                                ..before.clone()
                            };
                            self.edit_font(doc, &before, &after);
                        }
                    }
                }
            }
            self.refresh_from_doc(doc);
            self.dirty = true;
            return true;
        }
        let rename_matches = self.renaming.as_ref().is_some_and(|rename| {
            let expected = match &rename.target {
                RenameTarget::Font(id) => format!("books.rename.font:{id}"),
                RenameTarget::Scheme(id) => format!("books.rename.scheme:{id}"),
            };
            expected == target
        });
        if rename_matches {
            let handled = self
                .renaming
                .as_mut()
                .is_some_and(|rename| rename.field.perform_action(action, &[], doc, path));
            if handled {
                self.poll_rename_outcome(doc);
                self.refresh_from_doc(doc);
                self.dirty = true;
            }
            return handled;
        }
        let hit_action = self
            .hits
            .iter()
            .rev()
            .find(|hit| self.semantic_target(&hit.action).as_deref() == Some(target))
            .map(|hit| hit.action.clone());
        let Some(hit_action) = hit_action else {
            return false;
        };
        match action {
            A11yAction::Focus | A11yAction::ScrollIntoView => {
                self.semantic_focus = Some(target.to_string());
                self.dirty = true;
                true
            }
            A11yAction::Activate | A11yAction::ToggleCheck => {
                self.semantic_focus = Some(target.to_string());
                self.dispatch(hit_action, doc);
                self.refresh_from_doc(doc);
                self.persist_progress(doc);
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.semantic_focus = None;
        }
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.semantic_focus.is_some()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.hits
            .iter()
            .rev()
            .any(|hit| self.semantic_target(&hit.action).is_some() && hit.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    /// A book being imported shows once it parses. A screenshot imports
    /// nothing, so a loading state it stages is the still it shows.
    fn has_pending_visual_work(&self) -> bool {
        (self.live && self.import_loading.is_some())
            || self.query_busy()
            || (self.view == View::Reader && self.layout_pending())
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.flaps_animating()
            || self.faces.is_moving(ui::motion::frame_now())
            || self.shelf_controls.is_moving()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.load_toast_deadline()
            .into_iter()
            .chain(self.next_query_placeholder_step(ui::time::now()))
            .min()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the loop pumping while a flap is mid-slide, a page turn is
        // easing in, or the floating page bubble is lingering/fading.
        self.faces.is_moving(ui::motion::frame_now())
            || self.shelf_controls.is_moving()
            || (!self.reduce_motion
                && (self.flaps_animating()
                    || self.page_turn.is_some_and(|pt| !pt.done(ui::time::now()))
                    || self.reader_bubble.needs_frame(ui::time::now())
                    || (self.view != View::Reader && self.awaiting_arrival())))
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
        if reduced {
            self.page_turn = None;
            let (toc, notes) = (self.toc_open(), self.notes_open());
            self.toc_drawer.snap(toc);
            self.notes_drawer.snap(notes);
            self.dirty = true;
        }
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl BooksApp {
    /// The books, chapters, annotations and remote books as search entries.
    pub(crate) fn book_search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.book_search.entries()
    }
}

impl BooksApp {
    /// Start `effect`, invoked as `invocation`, against the library's
    /// `entries`. Showing something answers at once; sharing or fetching a
    /// book answers once its bytes are in place, through
    /// [`Self::take_effect_result`].
    ///
    /// # Errors
    /// Why the effect does not apply to what this reader holds now: a book,
    /// chapter or block that is gone, another share or fetch in progress.
    pub(crate) fn begin_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        effect: crate::resource::BooksEffect,
        entries: &[crate::model::LibraryEntry],
    ) -> Result<ui::framework::Effected, String> {
        use crate::resource::BooksEffect;
        match effect {
            BooksEffect::Share { book } => {
                self.begin_share_book(&book)?;
                self.resource_invocation = Some(invocation);
                self.resource_lifecycle = Some(ResourceLifecycle::Share);
                return Ok(ui::framework::Effected::Pending);
            }
            BooksEffect::Download { book } => {
                self.begin_hydrate_shared_book(&book, entries)?;
                self.resource_invocation = Some(invocation);
                self.resource_lifecycle = Some(ResourceLifecycle::Hydrate);
                return Ok(ui::framework::Effected::Pending);
            }
            BooksEffect::PreviousPage => self.on_prev_page(),
            BooksEffect::NextPage => self.on_next_page(),
            BooksEffect::Contents => self.on_toggle_toc(),
            BooksEffect::Notes => self.on_toggle_notes(),
            BooksEffect::Settings => self.on_open_settings(),
            BooksEffect::Open {
                book,
                chapter,
                page,
                block,
            } => self.reveal(&book, chapter, page, block)?,
            BooksEffect::GoTo {
                book,
                chapter,
                block,
            } => {
                let chapter = chapter.unwrap_or_else(|| self.open_chapter_of(&book));
                self.reveal(&book, chapter, 0, Some(block.unwrap_or(0)))?;
            }
            BooksEffect::SelectPassage {
                book,
                chapter,
                start_block,
                start_char,
                end_block,
                end_char,
            } => {
                let chapter = chapter.unwrap_or_else(|| self.open_chapter_of(&book));
                self.select_passage(
                    &book,
                    chapter,
                    (start_block, start_char.unwrap_or(0)),
                    (end_block.unwrap_or(start_block), end_char),
                )?;
            }
            BooksEffect::ReadChapter { book, chapter } => {
                let chapter = chapter.unwrap_or_else(|| self.open_chapter_of(&book));
                return self
                    .chapter_read(&book, chapter)
                    .map(ui::framework::Effected::value);
            }
            BooksEffect::OpenBook | BooksEffect::OpenStored { .. } => {
                return Err("opening a book is the surface's".into());
            }
        }
        self.record_progress();
        Ok(ui::framework::Effected::done())
    }

    /// Start the effect a published operation's `payload` names against
    /// `doc`'s library, as the host does when it is invoked.
    #[cfg(test)]
    pub(crate) fn start_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
        doc: &AutoCommit,
    ) -> Result<ui::framework::Effected, String> {
        let effect =
            <crate::resource::BooksEffect as ui::tilo_resource::Effect>::from_payload(payload)?;
        let entries: Vec<crate::model::LibraryEntry> =
            <Ebook as ui::tilo_resource::Resource>::read(doc)
                .map(|record| record.entries.values().cloned().collect())
                .unwrap_or_default();
        self.begin_effect(invocation, effect, &entries)
    }

    /// What chapter `chapter` of `book_id` says, block by block, with the
    /// book's title, author and chapter titles.
    fn chapter_read(&self, book_id: &str, chapter: usize) -> Result<serde_json::Value, String> {
        let book = self
            .library
            .iter()
            .find(|book| book.id == book_id)
            .ok_or("book is no longer in the library")?;
        let read = book
            .chapters
            .get(chapter)
            .ok_or("chapter is not in this book")?;
        Ok(serde_json::json!({
            "book": book.id,
            "title": book.title,
            "author": book.author,
            "chapter": chapter,
            "chapter_title": read.title,
            "chapters": book.chapters.iter().map(|chapter| chapter.title.as_str()).collect::<Vec<_>>(),
            "blocks": read.blocks.iter().enumerate().map(|(index, block)| serde_json::json!({
                "block": index,
                "text": block.plain_text(),
            })).collect::<Vec<_>>(),
        }))
    }

    /// The chapter this reader has open in `book`, else its first.
    fn open_chapter_of(&self, book: &str) -> usize {
        let open = self.view == View::Reader
            && self
                .library
                .get(self.book_idx)
                .is_some_and(|open| open.id == book);
        if open { self.chapter } else { 0 }
    }

    /// Show `book` at `chapter`, `page` and `block`. A place from an old
    /// search hit or row is not clamped to something nearby: a peer may have
    /// replaced the book, and opening a neighbouring passage would mislead.
    fn reveal(
        &mut self,
        book: &str,
        chapter: usize,
        page: usize,
        block: Option<usize>,
    ) -> Result<(), String> {
        let Some(index) = self.library.iter().position(|open| open.id == book) else {
            return Err("book is no longer in the library".into());
        };
        let Some(open) = self.library.get(index) else {
            return Err("book is no longer in the library".into());
        };
        if open.chapters.is_empty() {
            if chapter != 0 || block.is_some() {
                return Err("book no longer contains the requested reading target".into());
            }
        } else {
            let Some(chapter_item) = open.chapters.get(chapter) else {
                return Err("chapter is no longer available in this book".into());
            };
            if let Some(block) = block
                && block >= chapter_item.blocks.len()
            {
                return Err("annotation target is no longer available in this chapter".into());
            }
        }
        self.book_idx = index;
        self.view = View::Reader;
        self.chapter = chapter;
        self.page = page;
        self.pending_jump_block = block;
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        // Explicit, like `select_passage`: persisted, never re-adopted. A
        // book opened here from the shelf must not then take up its stored
        // position over the place asked for.
        self.adopt_position = false;
        self.retire_layout();
        self.dirty = true;
        Ok(())
    }

    /// The outcome of the share or fetch invoked as `invocation`, once it
    /// settled; taken once.
    pub(crate) fn take_effect_result(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if !self
            .completed_resource_invocation
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return None;
        }
        self.resource_lifecycle = None;
        self.completed_resource_invocation.take()
    }

    /// Stop the share or fetch invoked as `invocation`, as its caller asked.
    /// Parsing cannot be interrupted mid-archive, but its result is no
    /// longer taken: the loading generation it belongs to is gone.
    pub(crate) fn cancel_effect(&mut self, invocation: tilo_actions::InvocationId) -> bool {
        if self.resource_invocation != Some(invocation) {
            return false;
        }
        self.resource_invocation = None;
        self.pending_share = None;
        self.resource_lifecycle = None;
        self.import_loading = None;
        self.dirty = true;
        true
    }

    /// Answer `invocation`'s caller through `reply` once its work settles.
    pub(crate) fn await_effect(
        &mut self,
        invocation: tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.effect_replies.push((invocation, reply));
    }

    /// Answer every waiting caller whose share or fetch settled, and stop the
    /// ones their caller cancelled.
    fn deliver_effect_replies(&mut self) {
        for (invocation, reply) in std::mem::take(&mut self.effect_replies) {
            if reply.is_cancelled() {
                self.cancel_effect(invocation);
                continue;
            }
            match self.take_effect_result(invocation) {
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                    reply.answer(Ok(output));
                }
                Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                    reply.answer(Err(error));
                }
                Some(_) => reply.answer(Ok(tilo_actions::ActionOutput::default())),
                None => self.effect_replies.push((invocation, reply)),
            }
        }
    }

    /// Where the reader is in the open book, as the row it publishes.
    pub(crate) fn reading_row(&self) -> Option<ui::framework::LiveRow> {
        self.current_book_resource_context().map(|ctx| {
            ui::framework::LiveRow::from_resource(crate::actions::reading_resource(&ctx))
        })
    }

    /// The book, chapter, annotation or location a search hit names, with
    /// what opens it. Its edits are the library's own, on the published row.
    pub(crate) fn search_row(&self, id: &str) -> Option<ui::framework::LiveRow> {
        self.book_search
            .resolve(id)
            .map(ui::framework::LiveRow::from_resource)
    }

    /// Where a search hit or an agent's `open` queues what to show.
    pub(crate) fn opens(&self) -> Rc<std::cell::RefCell<Vec<String>>> {
        Rc::clone(&self.opens)
    }

    /// Show what was asked to be opened: the book, chapter, annotation or
    /// match a hit is anchored at, fetching a collaborator's book first.
    fn drain_opens(&self) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for anchor in opens {
            if let Some(effect) = self.book_search.opened(&anchor) {
                self.dispatch.effect(&effect);
            } else {
                tracing::debug!(%anchor, "books: a search hit to open is gone");
            }
        }
    }
}

impl BooksApp {
    fn forward_shared_input_event(&mut self, ctx: &mut InputEventCtx<'_>) -> bool {
        let mut input_handled = false;
        // The editors write the text as it is typed; finishing only closes.
        if self
            .comment_overlay
            .handle_event(&mut ctx.reborrow(&mut input_handled))
        {
            input_handled = true;
        }
        if self
            .note_composer
            .handle_event(&mut ctx.reborrow(&mut input_handled))
        {
            self.finish_composing(&mut ctx.state);
            input_handled = true;
        }

        let settings_actions = self
            .reading_settings
            .handle_event(&mut ctx.reborrow(&mut input_handled));
        for action in settings_actions {
            self.dispatch_reading_settings(action, &mut ctx.state);
        }

        let changes = self
            .number_controls
            .handle_event(&mut ctx.reborrow(&mut input_handled));
        for (field, value) in changes {
            match field {
                NumberField::FontScale => {
                    self.bump_setting(&mut ctx.state, |settings| {
                        settings.font_scale = (value / 100.0) as f32;
                    });
                }
                NumberField::LineSpacing => {
                    self.bump_setting(&mut ctx.state, |settings| {
                        settings.line_spacing = value as f32;
                    });
                }
                NumberField::FontMult(id) => {
                    if let Some(before) = self.fonts.iter().find(|font| font.id == id).cloned() {
                        let after = FontEntry {
                            size_mult: (value / 100.0) as f32,
                            ..before.clone()
                        };
                        self.edit_font(&mut ctx.state, &before, &after);
                    }
                }
            }
        }
        if input_handled {
            self.refresh_from_doc(ctx.state.doc());
            self.dirty = true;
            *ctx.handled = true;
        }
        input_handled
    }

    /// The dropdowns on screen take their events first while open or
    /// focused, and report the value the person picked. Returns whether one
    /// took the event.
    fn forward_dropdown_event(&mut self, ctx: &mut InputEventCtx<'_>) -> bool {
        let mut consumed = false;
        match self.view {
            View::Library => {
                let (taken, picked) = self.shelf_controls.handle_event(ctx);
                consumed = taken;
                if let Some(sort) = picked {
                    self.on_set_sort(sort, &mut ctx.state);
                }
            }
            View::Settings => {
                consumed = self.font_dropdown.handle_event(ctx);
                if let Some(id) = self.font_dropdown.take_change() {
                    self.on_set_default_font(&id, &mut ctx.state);
                    self.refresh_from_doc(ctx.state.doc());
                }
            }
            View::Reader | View::Search => {}
        }
        if consumed {
            self.dirty = true;
            *ctx.handled = true;
        }
        consumed
    }

    /// An in-progress inline rename owns input first: forward the raw event to
    /// its embedded field, then persist any commit/cancel it produced. Returns
    /// `true` when the field consumed the event (the caller stops there); a
    /// press outside the field blur-commits and falls through to the rest of
    /// `handle_event`.
    fn forward_rename_event(&mut self, ctx: &mut InputEventCtx<'_>) -> bool {
        if self.renaming.is_none() {
            return false;
        }
        let mut field_handled = false;
        let mut field_focus = None;
        if let Some(rn) = self.renaming.as_mut() {
            rn.field.handle_event(
                ctx.event,
                &mut ctx.widget_ctx(&mut field_handled, &mut field_focus),
            );
        }
        self.poll_rename_outcome(&mut ctx.state);
        if field_handled {
            self.refresh_from_doc(ctx.state.doc());
            self.dirty = true;
            *ctx.handled = true;
            return true;
        }
        false
    }

    fn handle_cursor_moved(
        &mut self,
        p: Point,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        if p == self.cursor {
            return;
        }
        self.cursor = p;
        self.reading_settings.set_cursor(p);
        self.dirty = true; // hover feedback
        if self.progress_dragging {
            self.on_seek_progress(self.progress_fraction_at(p));
            *handled = true;
            return;
        }
        // Margin-marker drag: a release past the threshold
        // re-anchors the highlight (vs a plain tap → comments).
        if self.mouse_down
            && let Some(md) = self.marker_drag.as_mut()
        {
            md.update((p.x, p.y));
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
                        DragUnit::Line => {
                            let (ls, le) = self.line_span_at(raw);
                            ui::extend_span_drag(lo, hi, raw, ls, le)
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

    /// Early-out hit-testing for the reading-settings popover, color picker
    /// and note composer, which each own a private hit stack outside the
    /// app-wide `self.hits` list and must be asked before it. Returns
    /// whether any of them consumed the press, so the caller can return
    /// immediately without falling through.
    fn handle_left_press_overlay(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if let Some(action) = self.reading_settings.hit_test(self.cursor) {
            self.dispatch_reading_settings(action, doc);
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
            return true;
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
            return true;
        }
        false
    }

    /// A press on a highlight's margin marker begins a re-anchor drag,
    /// taking priority over text selection. A press that doesn't move opens
    /// the highlight's comments on release instead.
    fn begin_marker_drag(&mut self) -> bool {
        if self.view != View::Reader {
            return false;
        }
        let Some((group, origin)) = self.marker_at(self.cursor) else {
            return false;
        };
        self.marker_drag = Some(ui::MarkerDrag::begin(
            group.to_string(),
            (self.cursor.x, self.cursor.y),
            origin,
        ));
        self.marker_group = Some(group);
        self.dirty = true;
        true
    }

    /// A press in a page margin that holds no highlight marker turns the
    /// page, as a tap on a printed page's edge would.
    fn turn_page_from_margin(&mut self, doc: &mut ui::widget::Edits<'_>) -> bool {
        if self.marker_at(self.cursor).is_some() {
            return false;
        }
        let Some(turn) = self.page_turn_margin_at(self.cursor) else {
            return false;
        };
        self.dispatch(turn, doc);
        self.refresh_from_doc(doc);
        self.persist_progress(doc);
        self.dirty = true;
        true
    }

    fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        if self.handle_left_press_overlay(doc, handled) {
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
        // The bubble is drawn below subsequent reader overlays, whose hits
        // keep precedence where geometry overlaps; but it is drawn over the
        // progress slider it shares the foot with, so its chevrons win there.
        let bubble_action = if comment_action.is_none()
            && shelf_action.is_none()
            && top
                .as_ref()
                .is_none_or(|hit| matches!(hit.action, Action::SeekProgress))
        {
            self.reader_bubble.hit_test(self.cursor)
        } else {
            None
        };
        if comment_action.is_none()
            && shelf_action.is_none()
            && top.is_none()
            && bubble_action.is_none()
            && self.turn_page_from_margin(doc)
        {
            *handled = true;
            return;
        }
        self.mouse_down = true;
        self.dragging_sel = false;
        self.press_pos = self.cursor;
        self.shelf_focus_visible = false;

        if bubble_action.is_none()
            && matches!(
                top.as_ref().map(|hit| &hit.action),
                Some(Action::SeekProgress)
            )
        {
            self.progress_dragging = true;
            self.on_seek_progress(self.progress_fraction_at(self.cursor));
            self.dirty = true;
            *handled = true;
            return;
        }

        if comment_action.is_none() && top.is_none() && self.begin_marker_drag() {
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
                // Double click selects a word and preserves word granularity
                // while dragging.
                Granularity::Word => {
                    if let Some(dp) = self.press_doc {
                        self.select_word_at(dp);
                        if let Some(s) = self.sel {
                            self.drag_anchor_span = s.ordered();
                            self.drag_unit = DragUnit::Word;
                            consumed_page = true;
                        }
                    }
                }
                // Triple click selects the complete logical flow block and
                // expands subsequent dragging block-by-block.
                Granularity::Line => {
                    if let Some(dp) = self.press_doc {
                        let (start, end) = self.line_span_at(dp);
                        self.sel = Some(TextSel {
                            anchor: start,
                            head: end,
                        });
                        self.selected_block = Some(dp.block);
                        self.drag_anchor_span = (start, end);
                        self.drag_unit = DragUnit::Line;
                        consumed_page = true;
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

    fn handle_left_release(&mut self, doc: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        self.mouse_down = false;
        if self.progress_dragging {
            self.progress_dragging = false;
            self.persist_progress(doc);
            self.refresh_from_doc(doc);
            self.dirty = true;
            *handled = true;
            return;
        }
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
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        // A dominantly horizontal gesture turns pages (scroll-right
        // advances). On this stack a physical scroll-right arrives as
        // a positive x, so the horizontal axis passes through as-is;
        // only the vertical is negated to make a downward scroll "forward".
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                // A notch travels as far as on the toolkit's other scroll
                // surfaces, so a mouse wheel feels the same in every app.
                let line = f64::from(Metrics::BASE.wheel_line);
                (f64::from(x) * line, f64::from(-y) * line)
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
    fn handle_library_key(&mut self, key: &Key, doc: &mut ui::widget::Edits<'_>) -> bool {
        // Navigation is over the shelf's display order (filtered + sorted),
        // resolved by the last `draw_library`.
        let n = self.shelf_order.len();
        if n == 0 {
            return false;
        }
        let last = n - 1;
        let cols = self.shelf_cols.max(1);
        let cursor = self.shelf_cursor.min(last);
        if !matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) {
            self.shelf_focus_visible = true;
        }
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

    fn reader_shortcut_matches(&self, shortcut: ReaderShortcut, pressed: &str) -> bool {
        self.reader_keys
            .shortcut(shortcut.id(), shortcut.default_key())
            .eq_ignore_ascii_case(pressed)
    }

    /// The shortcut recorder lives above all other keyboard routing, so a
    /// captured `b`, `m`, or `Esc` cannot accidentally annotate the book while
    /// its settings row says it is waiting for a key.
    fn handle_shortcut_capture_key(&mut self, key: &Key) -> bool {
        let Some(shortcut) = self.shortcut_capture else {
            return false;
        };
        match key {
            Key::Named(NamedKey::Escape) => {
                self.shortcut_capture = None;
                self.shortcut_capture_error = None;
            }
            Key::Character(character) => {
                if self
                    .reader_keys
                    .set_shortcut(shortcut.id(), character.as_str())
                {
                    self.shortcut_capture = None;
                    self.shortcut_capture_error = None;
                } else if crate::reader_keys::reserved_reader_key(character.as_str()) {
                    self.shortcut_capture_error = Some(format!(
                        "{} is reserved for a built-in reader command. Choose another key.",
                        key_legend(character.as_str())
                    ));
                } else {
                    self.shortcut_capture_error =
                        Some("Choose one unused printable key.".to_string());
                }
            }
            _ => {}
        }
        true
    }

    /// Keyboard handling. Returns `true` if the key was consumed (so the App's
    /// focus-navigation fallback doesn't also steal the arrows).
    fn handle_key(
        &mut self,
        key: &Key,
        mods: ModifiersState,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        // An in-progress inline rename owns the keyboard through its embedded
        // field via `handle_event`; no key handling for it belongs here.

        if self.shortcut_capture.is_some() {
            return self.handle_shortcut_capture_key(key);
        }

        if matches!(key, Key::Named(NamedKey::Tab)) && self.composing.is_none() {
            self.focus_next_semantic(mods.shift_key());
            return true;
        }
        if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space))
            && let Some(target) = self.semantic_focus.clone()
        {
            return Widget::perform_semantic_action(
                self,
                &target,
                &A11yAction::Activate,
                doc,
                &automerge::ROOT,
            );
        }

        if self.view == View::Settings {
            return self.handle_settings_escape_key(key, doc);
        }

        // The note's shared TextInput owns editing. Escape remains app-level
        // policy so it can finish and dismiss the floating composer.
        if self.composing.is_some() {
            return self.handle_composing_key(key, doc);
        }

        if self.view == View::Library {
            return self.handle_library_key(key, doc);
        }

        if self.view == View::Search {
            return self.handle_query_key(key, doc);
        }

        if self.handle_focused_rail_key(key).is_some() {
            return true;
        }

        self.handle_reader_key(key, mods, doc)
    }

    fn focus_next_semantic(&mut self, reverse: bool) {
        fn collect(node: &AccessNode, targets: &mut Vec<String>) {
            if let Some(target) = &node.action_target {
                targets.push(target.clone());
            }
            for child in &node.children {
                collect(child, targets);
            }
        }
        let Some(tree) = Widget::accessibility(self) else {
            return;
        };
        let mut targets = Vec::new();
        collect(&tree, &mut targets);
        targets.dedup();
        if targets.is_empty() {
            self.semantic_focus = None;
            return;
        }
        let current = self
            .semantic_focus
            .as_ref()
            .and_then(|focused| targets.iter().position(|target| target == focused));
        let index = match (current, reverse) {
            (Some(0) | None, true) => targets.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % targets.len(),
            (None, false) => 0,
        };
        self.semantic_focus = targets.get(index).cloned();
        self.dirty = true;
    }

    fn handle_settings_escape_key(&mut self, key: &Key, doc: &mut ui::widget::Edits<'_>) -> bool {
        if matches!(key, Key::Named(NamedKey::Escape)) {
            if self.color_picker.is_open() {
                self.color_picker.cancel();
            } else if self.font_picker {
                self.font_picker = false;
            } else if self.font_dropdown.is_open() {
                self.font_dropdown.close();
            } else if self.editing_scheme.is_some() {
                self.editing_scheme = None;
            } else {
                self.dispatch(Action::CloseSettings, doc);
            }
            return true;
        }
        false
    }

    fn handle_composing_key(&mut self, key: &Key, doc: &mut ui::widget::Edits<'_>) -> bool {
        matches!(key, Key::Named(NamedKey::Escape)) && self.finish_composing(doc)
    }

    fn handle_progress_slider_key(&mut self, key: &Key, doc: &mut ui::widget::Edits<'_>) -> bool {
        if self.semantic_focus.as_deref() != Some("books.reader.progress") {
            return false;
        }
        let target = match key {
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => {
                Some(self.reading_progress - 0.05)
            }
            Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => {
                Some(self.reading_progress + 0.05)
            }
            Key::Named(NamedKey::Home) => Some(0.0),
            Key::Named(NamedKey::End) => Some(1.0),
            _ => None,
        };
        if let Some(target) = target {
            self.on_seek_progress(target);
            self.persist_progress(doc);
            true
        } else {
            false
        }
    }

    fn handle_reader_key(
        &mut self,
        key: &Key,
        mods: ModifiersState,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.handle_progress_slider_key(key, doc) {
            return true;
        }
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
                let toc_overlay = self.toc_drawer.is_overlay() && self.toc_open();
                let notes_overlay = self.notes_drawer.is_overlay() && self.notes_open();
                if self.comment_overlay.is_open() {
                    self.dispatch(Action::CloseComments, doc);
                } else if self.reading_settings.is_open() {
                    self.reading_settings.close();
                } else if toc_overlay || notes_overlay {
                    self.dispatch(Action::DismissOverlay, doc);
                } else {
                    self.dispatch(Action::Back, doc);
                }
                true
            }
            Key::Character(c) => {
                let action = if self.reader_shortcut_matches(ReaderShortcut::PreviousPage, c) {
                    Some(Action::PrevPage)
                } else if self.reader_shortcut_matches(ReaderShortcut::NextPage, c) {
                    Some(Action::NextPage)
                } else if self.reader_shortcut_matches(ReaderShortcut::Bookmark, c) {
                    Some(Action::ToggleBookmark)
                } else if self.reader_shortcut_matches(ReaderShortcut::Highlight, c) {
                    self.sel
                        .filter(|s| !s.is_empty())
                        .map(|_| Action::SetColor(self.hi_color))
                } else if self.reader_shortcut_matches(ReaderShortcut::AddNote, c) {
                    Some(Action::AddNote)
                } else {
                    match c.as_str() {
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
                        "c" | "C" => self
                            .sel
                            .filter(|s| !s.is_empty())
                            .map(|_| Action::CopySelection),
                        _ => None,
                    }
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
        reason = "each iteration subtracts a fixed swipe step and breaks immediately \
                  if a turn made no progress, so the loop is bounded regardless of float \
                  imprecision"
    )]
    fn handle_swipe(&mut self, dx: f64, doc: &mut ui::widget::Edits<'_>) -> bool {
        if dx == 0.0 || self.view != View::Reader {
            return false;
        }
        if self.reading_settings.is_open() || self.composing.is_some() {
            return false;
        }
        // A swipe turns the page once it travels as far as any swipe that
        // commits: further than a wheel step, so it reads as a deliberate flick.
        let step = f64::from(Metrics::BASE.swipe_commit);
        self.swipe_accum += dx;
        let mut turned = false;
        while self.swipe_accum >= step {
            self.swipe_accum -= step;
            let before = (self.chapter, self.page);
            self.dispatch(Action::NextPage, doc);
            if (self.chapter, self.page) == before {
                self.swipe_accum = 0.0;
                break;
            }
            turned = true;
        }
        while self.swipe_accum <= -step {
            self.swipe_accum += step;
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
    fn handle_wheel(&mut self, dy: f64, doc: &mut ui::widget::Edits<'_>) -> bool {
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
            View::Search => self.query.scroll_by(dy),
            View::Reader => {
                // A page fills the viewport, so the wheel turns pages rather than
                // scrolling within one. Suppressed while a modal surface (the
                // reading-settings popover or an in-progress note) owns input.
                if self.reading_settings.is_open() || self.composing.is_some() {
                    return false;
                }
                // Contents and annotations are independently scrollable.  Do
                // this before the page handler: a pointer over an open rail
                // must never advance the book in the background.
                if self.scroll_reader_rail(dy).is_some() {
                    return true;
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

// --- Painted controls ---------------------------------------------------

/// The hit rect for a small painted control: the control's own rect grown
/// symmetrically to at least `metrics.min_pointer_target` on each side, so a
/// switch track is still a real pointer target without painting bigger.
pub(crate) fn pointer_target(rect: Rect, theme: &Theme) -> Rect {
    let min = f64::from(theme.tokens.metrics.min_pointer_target);
    let dx = ((min - rect.width()) / 2.0).max(0.0);
    let dy = ((min - rect.height()) / 2.0).max(0.0);
    rect.inflate(dx, dy)
}

/// The track of a painted switch: the retained `Toggle`'s proportions in
/// the theme's spacing steps.
pub(crate) fn switch_size(theme: &Theme) -> (f64, f64) {
    let spacing = &theme.tokens.spacing;
    (f64::from(spacing.s8 + spacing.s3), f64::from(spacing.s6))
}

/// The quiet scroll thumb along the trailing edge of a painted scroll area.
fn paint_scroll_thumb(ctx: &mut PaintCtx<'_>, area: Rect, metrics: ScrollMetrics) {
    if !metrics.overflows() {
        return;
    }
    // The bar sits in the lane a `Scrollable` keeps on its trailing edge.
    let gutter = f64::from(ctx.theme.tokens.metrics.scrollbar_gutter);
    let lane = Rect::new(area.x1 - gutter, area.y0, area.x1, area.y1);
    let bar = Scrollbar::vertical();
    let track = bar.track_in(lane, ctx.theme);
    bar.draw(ctx.scene, ctx.transform, track, metrics, ctx.theme, false);
}

/// The width of a button face of `size` showing `label` (and `icon`).
fn button_width(ctx: &mut PaintCtx<'_>, label: &str, icon: Option<Icon>, size: ControlSize) -> f64 {
    let mut face = ButtonFace::new(Rect::ZERO, label).control_size(size);
    if let Some(icon) = icon {
        face = face.icon(icon);
    }
    face.intrinsic_width(ctx.text, ctx.theme)
}

impl BooksApp {
    /// Paint a button face and register its pointer target for `action`.
    fn button(&mut self, ctx: &mut PaintCtx<'_>, face: ButtonFace<'_>, action: Action) {
        let theme = ctx.theme;
        let hit = face.hit_rect(theme);
        let face = face.hovered(hit.contains(self.cursor));
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        self.push_hit(hit, action);
    }

    /// An icon-only button of `size` centred on `center`; `active` shows a
    /// toggle that is on. Returns its rect.
    fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        center: Point,
        size: ControlSize,
        (icon, name): (Icon, &str),
        active: bool,
        action: Action,
    ) -> Rect {
        let extent = size.height(ctx.theme);
        let rect = Rect::from_center_size(center, (extent, extent));
        self.button(
            ctx,
            ButtonFace::icon_only(rect, icon, name)
                .control_size(size)
                .pressed(active),
            action,
        );
        rect
    }

    /// Draw a shared switch face and retain the app-specific action routing.
    /// `key` names the switch, so it slides when a click turns it.
    fn switch(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        key: impl std::hash::Hash,
        track: Rect,
        on: bool,
        action: Action,
    ) {
        let theme = ctx.theme;
        let hit = pointer_target(track, theme);
        Toggle::paint(
            ctx.scene,
            ctx.transform,
            theme,
            &ToggleFace::new(track, on)
                .pose(self.faces.switch(key, on, theme.motion()))
                .hovered(hit.contains(self.cursor)),
        );
        self.push_hit(hit, action);
    }

    /// A switch whose trailing edge is `x1`, centred on `cy`. Returns its
    /// track.
    fn switch_at(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        key: impl std::hash::Hash,
        x1: f64,
        cy: f64,
        on: bool,
        action: Action,
    ) -> Rect {
        let (w, h) = switch_size(ctx.theme);
        let track = Rect::new(x1 - w, cy - h / 2.0, x1, cy + h / 2.0);
        self.switch(ctx, key, track, on, action);
        track
    }
}

// --- Library (shelf) view ---------------------------------------------------

/// The shelf grid as laid out this frame: columns of portrait media cards
/// under the page title, scrolled by the shelf offset.
struct ShelfGrid {
    cols: usize,
    card_w: f64,
    /// Column gap and row gap (the media grid's rhythm).
    column_gap: f64,
    row_gap: f64,
    start_x: f64,
    /// Top of the first row, already scrolled.
    start_y: f64,
    /// Each row's text-block height (the tallest card of the row), so every
    /// title in a row starts on the same line.
    row_text: Vec<f64>,
}

impl ShelfGrid {
    fn cover_h(&self) -> f64 {
        self.card_w / MediaAspect::Portrait.ratio()
    }

    fn row_top(&self, row: usize) -> f64 {
        let cover = self.cover_h();
        self.start_y
            + self
                .row_text
                .iter()
                .take(row)
                .map(|text| cover + text + self.row_gap)
                .sum::<f64>()
    }

    /// The card rect of grid slot `i` (in display order).
    fn cell(&self, i: usize) -> Rect {
        let col = i % self.cols;
        let row = i / self.cols;
        let x = self.start_x + col as f64 * (self.card_w + self.column_gap);
        let y = self.row_top(row);
        let text = self.row_text.get(row).copied().unwrap_or(0.0);
        Rect::new(x, y, x + self.card_w, y + self.cover_h() + text)
    }

    /// Height of every row, gaps between them included.
    fn height(&self) -> f64 {
        let rows = self.row_text.len();
        if rows == 0 {
            return 0.0;
        }
        self.row_top(rows) - self.start_y - self.row_gap
    }
}

impl BooksApp {
    /// The header row of a full-width page (library, settings): no fill,
    /// one hairline under it. Returns the band.
    fn draw_page_header(&self, ctx: &mut PaintCtx<'_>) -> Rect {
        let r = self.rect;
        let band = Rect::new(r.x0, r.y0, r.x1, r.y0 + self.header_h);
        ui::widget::toolbar::paint_band(ctx.scene, ctx.transform, ctx.theme, band);
        band
    }

    /// A page's title block at the content gutter: the page name in
    /// `LargeTitle` and, when there is one, a count in `Caption` under it.
    /// Returns the y just below the block.
    fn draw_page_title(
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        title: &str,
        count: Option<&str>,
    ) -> f64 {
        let theme = ctx.theme;
        let mut y = origin.y;
        ctx.text_role(
            title,
            TextRole::LargeTitle,
            theme.text_primary,
            origin,
            None,
        );
        y += ctx.line_box(TextRole::LargeTitle);
        if let Some(count) = count {
            y += f64::from(theme.tokens.spacing.rhythm.tight);
            let style = theme.text(TextRole::Caption).numeric();
            ctx.text(count, &style, theme.text_dim, Point::new(origin.x, y), None);
            y += ctx.line_box(TextRole::Caption);
        }
        y
    }

    fn draw_library(&mut self, ctx: &mut PaintCtx<'_>) {
        let lib = self.shelf_books();
        let r = self.rect;
        let band = self.draw_page_header(ctx);
        // An empty shelf offers "Open a book" as its one action, so the
        // header does not repeat it.
        self.draw_library_actions(ctx, band, !lib.is_empty());
        if !lib.is_empty() {
            let inset = f64::from(ctx.theme.tokens.spacing.s3);
            let cursor = self.cursor;
            let prefs = self.library_prefs;
            self.shelf_controls
                .draw_bar(ctx, band, band.x0 + inset, cursor, prefs);
        }

        let body = Rect::new(r.x0, band.y1, r.x1, r.y1);
        if lib.is_empty() {
            self.draw_empty_library(ctx, body);
            return;
        }

        let order = self.shelf_display_order(&lib);
        self.shelf_order.clone_from(&order);
        self.draw_library_body(ctx, &lib, &order, body);
    }

    /// The trailing actions of the library header: Settings, and (with
    /// `open`) the one primary action of the screen, "Open book", a way in
    /// that does not depend on any folder.
    fn draw_library_actions(&mut self, ctx: &mut PaintCtx<'_>, band: Rect, open: bool) {
        let theme = ctx.theme;
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let y = band.center().y - height / 2.0;
        let mut x1 = band.x1 - f64::from(theme.tokens.spacing.s3);
        if open {
            let label = "Open book";
            let width = ButtonFace::new(Rect::ZERO, label)
                .icon(Icon::Plus)
                .intrinsic_width(ctx.text, theme);
            let rect = Rect::new(x1 - width, y, x1, y + height);
            self.button(
                ctx,
                ButtonFace::new(rect, label)
                    .icon(Icon::Plus)
                    .role(ButtonRole::Primary),
                Action::OpenViaPortal,
            );
            x1 = rect.x0 - f64::from(theme.tokens.spacing.rhythm.related);
        }
        let settings = Rect::new(x1 - height, y, x1, y + height);
        self.button(
            ctx,
            ButtonFace::icon_only(settings, Icon::Settings, "Settings"),
            Action::OpenSettings,
        );
    }

    /// The scrolling body of a populated shelf: the page title, then the
    /// grid of covers and the trailing add tile, or the finished-books empty
    /// state when the filter hides everything.
    fn draw_library_body(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        lib: &[Arc<Book>],
        order: &[usize],
        body: Rect,
    ) {
        let theme = ctx.theme;
        let gutter = f64::from(theme.tokens.spacing.rhythm.group);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let count = match lib.len() {
            1 => "1 book".to_string(),
            n => format!("{n} books"),
        };
        let title_y = body.y0 + gutter - self.shelf_scroll;
        let title_bottom = Self::draw_page_title(
            ctx,
            Point::new(body.x0 + gutter, title_y),
            "Library",
            Some(&count),
        );
        let grid_top = title_bottom + gutter;

        if order.is_empty() {
            ctx.scene.pop_layer();
            self.shelf_max_scroll = 0.0;
            self.shelf_scroll = 0.0;
            let area = Rect::new(body.x0, grid_top, body.x1, body.y1);
            EmptyState::paint(
                ctx,
                area,
                &EmptyStateContent {
                    message: Some("Turn off Hide finished to see them again."),
                    ..EmptyStateContent::new("Every book here is finished")
                },
                self.cursor,
            );
            return;
        }

        let cards: Vec<ShelfCard<'_>> = order
            .iter()
            .enumerate()
            .filter_map(|(slot, &index)| {
                let book = lib.get(index)?;
                let notes = self.shelf_annos.get(&book.id).copied().unwrap_or(0);
                let here = index < self.library.len();
                Some(ShelfCard {
                    book,
                    open: Action::OpenBook(index),
                    here,
                    shared: if here {
                        self.library_paths
                            .get(&book.id)
                            .is_some_and(|path| is_shared(path))
                    } else {
                        self.shared_in_workspace(&book.id)
                    },
                    arriving: self.arriving(&book.id),
                    status: self.shelf_status(&book.id),
                    progress: self.shelf_fraction.get(&book.id).copied().unwrap_or(0.0),
                    notes: match notes {
                        0 => None,
                        1 => Some("1 note".to_string()),
                        n => Some(format!("{n} notes")),
                    },
                    focused: self.shelf_focus_visible && slot == self.shelf_cursor,
                })
            })
            .collect();
        let grid = self.shelf_grid(ctx, &cards, body, grid_top);
        let content_h = (grid_top + self.shelf_scroll - body.y0) + grid.height() + gutter;
        self.shelf_max_scroll = (content_h - body.height()).max(0.0);
        if self.scroll_to_cursor {
            let cell = grid.cell(self.shelf_cursor);
            if cell.y0 < body.y0 {
                self.shelf_scroll -= body.y0 - cell.y0;
            } else if cell.y1 > body.y1 {
                self.shelf_scroll += cell.y1 - body.y1;
            }
            self.scroll_to_cursor = false;
        }
        self.shelf_scroll = self.shelf_scroll.clamp(0.0, self.shelf_max_scroll);

        for (slot, card) in cards.iter().enumerate() {
            let cell = grid.cell(slot);
            if cell.y0 > body.y1 || cell.y1 < body.y0 {
                continue;
            }
            self.draw_book_card(ctx, card, cell, body);
        }
        let add = grid.cell(cards.len());
        if add.y0 <= body.y1 && add.y1 >= body.y0 {
            let tile = Rect::new(add.x0, add.y0, add.x1, add.y0 + grid.cover_h());
            self.draw_add_book_tile(ctx, tile, body);
        }
        ctx.scene.pop_layer();
        paint_scroll_thumb(
            ctx,
            body,
            ScrollMetrics::new(content_h, body.height(), self.shelf_scroll),
        );
    }

    /// Columns of portrait cards across the content width (one more slot
    /// for the add tile), with every row's text block as tall as its
    /// tallest card. Publishes the column count for keyboard row steps.
    fn shelf_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cards: &[ShelfCard<'_>],
        body: Rect,
        top: f64,
    ) -> ShelfGrid {
        let theme = ctx.theme;
        let rhythm = &theme.tokens.spacing.rhythm;
        let gutter = f64::from(rhythm.group);
        let (column_gap, row_gap) = (f64::from(rhythm.section), f64::from(rhythm.group));
        let avail = (body.width() - 2.0 * gutter).max(1.0);
        let cols = card_columns(avail, MediaSize::Small.extent(theme), column_gap);
        let card_w = card_column_width(avail, cols, column_gap);
        self.shelf_cols = cols;
        self.shelf_cursor = self.shelf_cursor.min(cards.len().saturating_sub(1));
        let mut row_text = cards
            .chunks(cols)
            .map(|row| {
                row.iter()
                    .map(|card| {
                        Self::book_face(card, MediaArt::Placeholder, &[])
                            .text_height(ctx.text, theme, card_w)
                    })
                    .fold(0.0, f64::max)
            })
            .collect::<Vec<_>>();
        if cards.len().is_multiple_of(cols) {
            // The add tile starts a row of its own: art only.
            row_text.push(0.0);
        }
        ShelfGrid {
            cols,
            card_w,
            column_gap,
            row_gap,
            start_x: body.x0 + gutter,
            start_y: top,
            row_text,
        }
    }

    /// The trailing add tile: a drop target for EPUB files that also opens
    /// the host file picker.
    fn draw_add_book_tile(&mut self, ctx: &mut PaintCtx<'_>, tile: Rect, body: Rect) {
        DropZoneFace::new("Add a book")
            .icon(Icon::Plus)
            .message("Drop an EPUB here or choose one")
            .drag_over(self.file_open.is_hovering())
            .paint(ctx, tile, 0.0);
        let hit = tile.intersect(body);
        if hit.height() > 0.0 {
            self.push_hit(hit, Action::OpenViaPortal);
        }
    }

    /// The empty shelf: one sentence and the one way in, opening a book from
    /// anywhere (no magic folder; the shelf fills itself from what is
    /// opened).
    fn draw_empty_library(&mut self, ctx: &mut PaintCtx<'_>, body: Rect) {
        let message = self
            .open_failure
            .as_deref()
            .unwrap_or("Books you open are kept here.");
        let content = EmptyStateContent {
            icon: Some(Icon::FileText),
            message: Some(message),
            action_label: Some("Open a book"),
            error: self.open_failure.is_some(),
            ..EmptyStateContent::new("Your library is empty")
        };
        if let Some(action) = self
            .file_open
            .paint(ctx, body, &content, self.cursor, false)
        {
            self.push_hit(action, Action::OpenViaPortal);
        }
    }

    /// The filtered + sorted display order of the shelf: original library
    /// indices, hiding finished books when requested and ordered by the chosen
    /// key (ties broken by title so the order is stable).
    fn shelf_display_order(&self, lib: &[Arc<Book>]) -> Vec<usize> {
        use crate::model::LibrarySort;
        // `i`/`a`/`b` below are always drawn from `0..lib.len()` (or from
        // `idx`, which is built from that same range), so every lookup is
        // in-bounds by construction — `.get()` still keeps the closures
        // panic-free for clippy without depending on that invariant.
        let frac = |i: usize| {
            lib.get(i).map_or(0.0, |b| {
                self.shelf_fraction.get(&b.id).copied().unwrap_or(0.0)
            })
        };
        let opened = |i: usize| {
            lib.get(i)
                .map_or(0, |b| self.shelf_opened.get(&b.id).copied().unwrap_or(0))
        };
        let title = |i: usize| {
            lib.get(i)
                .map_or_else(String::new, |b| b.title.to_lowercase())
        };
        let author = |i: usize| {
            lib.get(i)
                .map_or_else(String::new, |b| b.author.to_lowercase())
        };
        let mut idx: Vec<usize> = (0..lib.len())
            .filter(|&i| !self.library_prefs.hide_read || !crate::model::Progress::is_read(frac(i)))
            .collect();
        match self.library_prefs.sort {
            LibrarySort::Title => idx.sort_by_key(|&a| title(a)),
            LibrarySort::Author => {
                idx.sort_by(|&a, &b| author(a).cmp(&author(b)).then(title(a).cmp(&title(b))));
            }
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
        let img = ui::raster::rgba8(Blob::new(cover.rgba.clone()), cover.width, cover.height);
        self.cover_cache.insert(book.id.clone(), img.clone());
        Some(img)
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
    /// Top of the running header's line box and the y of the rule beneath it
    /// (framed page only; zero otherwise). The content box starts below the
    /// rule, so header, rule and first line never meet at any text scale.
    header_y: f64,
    rule_y: f64,
    /// The band at the page's foot the progress slider is centred in. The
    /// page bubble's slot lies between it and the content box.
    slider_band: f64,
}

impl BooksApp {
    fn draw_reader(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let lib = std::mem::take(&mut self.library);
        let annos = std::mem::take(&mut self.annotations);
        let r = self.rect;
        self.book_idx = self.book_idx.min(lib.len().saturating_sub(1));
        let nchap = lib.get(self.book_idx).map_or(0, |b| b.chapters.len());
        self.chapter = self.chapter.min(nchap.saturating_sub(1));

        let mode = self.settings.mode;
        // Measure the running header's line box at this frame's chrome scale
        // before the geometry reserves its slot (see `running_header_h`).
        self.running_header_h = ctx.line_box(TextRole::Caption);
        let geom = self.reader_page_geometry(r, mode);
        let palette = self.resolve_palette(theme);

        // ---- (re)compute the chapter layout -----------------------------
        let (base_family, font_mult, font_sig, base_mono) = self.body_font();
        let layout = match lib.get(self.book_idx) {
            Some(book) => self.resolve_chapter_layout(
                ctx.text,
                &chapter_layouts::LayoutInputs {
                    book,
                    geom: &geom,
                    font_sig,
                    base_family: &base_family,
                    font_mult,
                },
            ),
            None => ChapterLayout {
                blocks: Vec::new(),
                total_height: 0.0,
                pages: vec![0.0],
                key: LayoutKey::new(0, 0.0, 0.0, &self.settings, font_sig, 1.0),
                complete: true,
            },
        };
        self.resolve_pending_seek_fraction(&layout, mode, &geom);
        self.cur_page_count = layout.page_count();
        self.resolve_reader_page_index(&layout, mode, &geom);

        // ---- the on-screen reading columns (panes) ----------------------
        let panes = self.compute_panes(&layout, mode, &geom);

        // ---- page surface ------------------------------------------------
        self.draw_reader_page_surface(ctx, &lib, &palette, &geom);
        if !mode.is_continuous() {
            // Narrower than half a pointer target, a margin is not a
            // target anyone could aim at.
            let min = f64::from(ctx.theme.tokens.metrics.min_pointer_target) / 2.0;
            self.push_page_turn_margins(&geom, min);
        }

        // ---- draw each reading column (margins → selection bands → text) -
        self.reader_body = (geom.body_y0, geom.body_y1);
        self.sel_runs.clear();
        self.hi_markers.clear();
        self.hi_bands.clear();
        self.page_first_doc = None;
        self.page_last_doc = None;
        let frame = ReaderFrame {
            layout: &layout,
            annos: &annos,
            palette: &palette,
            base_family: &base_family,
            base_mono,
            chapter: self.chapter,
            sel_ord: self.sel.filter(|s| !s.is_empty()).map(|s| s.ordered()),
            comment_group: self.comment_overlay.group(),
        };
        let (sel_anchor, comment_box) = self.draw_reader_columns(ctx, &frame, &panes, &geom, mode);
        let line_pitch = f64::from(
            self.settings.body_size_with(font_mult)
                * 1.25
                * self.settings.line_spacing
                * ctx.text.text_scale(),
        );
        Self::draw_unset_text(ctx, &layout, &panes, &geom, (&palette, line_pitch));
        self.comment_overlay.set_anchor(comment_box);
        self.resolve_pending_sel_edge();

        // restore the layout cache (key drives reuse next frame)
        self.layout = (layout.complete || !layout.blocks.is_empty()).then_some(layout);
        // restore the annotation cache before chrome reads it (the top bar's
        // bookmark state + the notes drawer both consult `self.annotations`)
        self.annotations = annos;
        self.draw_reader_footer_and_chrome(ctx, lib, &geom, sel_anchor);
    }

    /// The page's side margins turn it, as a book's edges do: a click beside
    /// the text goes back on the leading side and forward on the trailing
    /// one ([`Self::page_turn_margin_at`]). The text column itself stays for
    /// selecting, and any control drawn over a margin wins there.
    fn push_page_turn_margins(&mut self, geom: &ReaderPageGeometry, min_width: f64) {
        let text_x1 = geom.full_content_x + f64::from(geom.full_content_w);
        let leading = Rect::new(
            geom.paper_x,
            geom.body_y0,
            geom.full_content_x,
            geom.body_y1,
        );
        let trailing = Rect::new(
            text_x1,
            geom.body_y0,
            geom.paper_x + geom.paper_w,
            geom.body_y1,
        );
        let usable =
            |rect: Rect| (rect.width() >= min_width && rect.height() > 0.0).then_some(rect);
        self.turn_margins = (usable(leading), usable(trailing));
    }

    /// The page turn a press at `p` asks for, when it lands in a side margin
    /// of a paginated page.
    fn page_turn_margin_at(&self, p: Point) -> Option<Action> {
        if self.view != View::Reader || self.settings.mode.is_continuous() {
            return None;
        }
        let (leading, trailing) = self.turn_margins;
        if leading.is_some_and(|rect| rect.contains(p)) {
            Some(Action::PrevPage)
        } else if trailing.is_some_and(|rect| rect.contains(p)) {
            Some(Action::NextPage)
        } else {
            None
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
    /// `self.library`, taken at the top of [`Self::draw_reader`].
    fn draw_reader_footer_and_chrome(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        lib: Arc<Vec<Arc<Book>>>,
        geom: &ReaderPageGeometry,
        sel_anchor: SelAnchor,
    ) {
        let theme = ctx.theme;
        let nchap = lib.get(self.book_idx).map_or(0, |b| b.chapters.len());
        // ---- page footer -------------------------------------------------
        // The slider sits where the reader is: the start of the page on
        // screen, the same position a seek to its value lands on.
        let frac = self.slider_position(nchap);
        self.reading_progress = frac;
        let paper_y1 = geom.paper_y + geom.paper_h;
        let slider_h = ControlSize::Small.height(theme);
        let slider_cy = paper_y1 - geom.slider_band / 2.0;
        let text = Rect::new(
            geom.full_content_x,
            slider_cy - slider_h / 2.0,
            geom.full_content_x + f64::from(geom.full_content_w),
            slider_cy + slider_h / 2.0,
        );
        // Where the reader is, in words, at the slider's end: the track alone
        // says roughly how far, not how much.
        let label = Rect::new(
            text.x1 - progress_label_width(ctx),
            text.y0,
            text.x1,
            text.y1,
        );
        let label_gap = f64::from(theme.tokens.spacing.rhythm.related);
        let text = Rect::new(text.x0, text.y0, label.x0 - label_gap, text.y1);
        // The track, not the control's knob room, runs from the margin like
        // the text above it.
        let knob_room = SliderFace::new(text, 0.0).track_rect().x0 - text.x0;
        let slider_rect = text.inflate(knob_room.max(0.0), 0.0);
        let face = SliderFace::new(slider_rect, f64::from(frac))
            .hovered(slider_rect.contains(self.cursor))
            .dragging(self.progress_dragging)
            .focused(self.semantic_focus.as_deref() == Some("books.reader.progress"));
        self.progress_slider_track = face.track_rect();
        // The slider stands on the page, so it takes the page's polarity,
        // at the window's UI scale and text size.
        let page_dark = self.resolve_palette(theme).dark;
        let page_theme = if page_dark == theme.tokens.colors.palette.dark {
            theme.clone()
        } else {
            if page_dark {
                Theme::dark()
            } else {
                Theme::light()
            }
            .with_ui_scale(theme.ui_scale)
            .with_text_scale(theme.text_scale)
        };
        // The foot's band: the slider's, and the page bubble's while a new
        // page is shown. Reaching for the slider gives it back.
        let band = Rect::new(
            geom.center_x0,
            paper_y1 - geom.slider_band,
            geom.center_x1,
            paper_y1,
        );
        let position = ReaderPosition {
            chapter: self.chapter,
            page: self.page,
            page_count: self.cur_page_count,
            chapter_count: nchap.max(1),
        };
        let slider_active = self.progress_dragging || band.contains(self.cursor);
        let bubble = self
            .reader_bubble
            .advance(position, slider_active, self.cursor);
        let slider_alpha = 1.0 - bubble;
        if slider_alpha > 0.0 {
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                slider_alpha,
                ctx.transform,
                &band,
            );
            Slider::paint(ctx.scene, ctx.transform, &page_theme, &face);
            draw_progress_label(ctx, label, frac, page_theme.text_dim);
            ctx.scene.pop_layer();
        }
        self.push_hit(slider_rect, Action::SeekProgress);
        self.reader_bubble.draw(ctx, band, self.cursor, position);

        // ---- chrome: header row, selection HUD, drawers
        self.draw_reader_header(ctx, &lib);
        // Floating selection toolbar over the page (under any overlay rail).
        self.draw_selection_hud(ctx, geom);
        self.draw_rails(ctx, &lib);
        // The on-page note composer (anchored near the passage) floats over the
        // page instead of shunting note-writing off to a separate screen.
        self.draw_note_composer(ctx, sel_anchor.map(|(_, y)| y));
        // The floating comment overlay for a clicked highlight (its own composer
        // lives inside it, so the standalone note composer above stands down).
        self.comment_overlay.draw(
            ctx,
            self.rect,
            Rect::new(geom.center_x0, geom.body_y0, geom.center_x1, geom.body_y1),
            self.cursor,
            &self.annotations,
            self.hi_color,
        );
        // Reading-settings floats above everything, anchored under its
        // button, and owns its own modal hit stack.
        self.reading_settings.draw(
            ctx,
            self.rect,
            self.type_button,
            &Appearance {
                settings: &self.settings,
                fonts: &self.fonts,
                default_font_id: &self.default_font_id,
                schemes: &self.schemes,
            },
        );

        self.library = lib;
    }

    /// Column, paper, and content-box geometry for one reader frame.
    /// Computed once per [`Self::draw_reader`] call from the current rect and
    /// the docked-rail reveal.
    fn reader_page_geometry(&self, r: Rect, mode: ReadMode) -> ReaderPageGeometry {
        // ---- the content column ------------------------------------------
        // A docked drawer insets the page (by its eased reveal); a floating
        // one lies over a full-width page behind its scrim.
        let column = self.notes_drawer.content_rect();
        let center_x0 = column.x0;
        let center_x1 = column.x1;
        let body_y0 = r.y0 + self.header_h;
        // The page renders to the window bottom: the page indicator floats as
        // a transient bubble (`ReaderBubble`) instead of a permanent strip.
        let body_y1 = r.y1;

        // ---- paper rect + content box -----------------------------------
        // In a compact window the page fills the reading area edge-to-edge,
        // no framed page or margin; a wide window keeps the framed page.
        let framed = self.rect.width() >= self.reader_compact_breakpoint;
        let center_w = (center_x1 - center_x0).max(120.0);
        let sp = self.reader_spacing;
        // The selection controls dock in the slot above the prose, which is
        // kept whether or not text is selected, so selecting never covers a
        // line or moves the text.
        let hud_slot = self.selection_hud_h + 2.0 * f64::from(sp.s2);
        // The foot is one band the progress slider and the page bubble take
        // turns in, so neither ever lies over the text.
        let slider_band = if framed {
            f64::from(sp.s12)
        } else {
            f64::from(sp.s8)
        }
        .max(self.bubble_h + f64::from(sp.s4));
        let pad_bot = slider_band;
        let (paper_x, paper_y, paper_w, paper_h, pad_x, pad_top, header_y, rule_y) = if framed {
            let margin = f64::from(sp.s6);
            let paper_w = (center_w - 2.0 * margin).clamp(120.0, PAGE_MAX_W);
            let paper_x = center_x0 + (center_w - paper_w) / 2.0;
            let paper_y = body_y0 + margin;
            let paper_h = (body_y1 - body_y0 - 2.0 * margin).max(120.0);
            // Running header, then the rule an `s1` under its measured
            // line box, then the content an `s2` under the rule. The slot
            // follows the header's real height, so at 200 % the header
            // grows away from the rule instead of into it (rounded so 1×
            // keeps its pixel grid).
            let header_y = paper_y + f64::from(sp.s6);
            let rule_y = (header_y + self.running_header_h + f64::from(sp.s1)).round();
            let pad_top = (rule_y - paper_y + f64::from(sp.s2)).max(hud_slot);
            let pad = f64::from(sp.s12);
            (
                paper_x, paper_y, paper_w, paper_h, pad, pad_top, header_y, rule_y,
            )
        } else {
            let paper_x = center_x0;
            let paper_y = body_y0;
            let paper_w = center_w;
            let paper_h = (body_y1 - body_y0).max(120.0);
            // Accessibility text needs a little extra horizontal breathing
            // room on an edge-to-edge page. The layout width follows this
            // inset, so words reflow before reaching the viewport edge.
            let pad_x = if self.settings.font_scale > 1.0 {
                f64::from(sp.s8)
            } else {
                f64::from(sp.s6)
            };
            (
                paper_x,
                paper_y,
                paper_w,
                paper_h,
                pad_x,
                hud_slot.max(f64::from(sp.s6)),
                0.0,
                0.0,
            )
        };
        let full_content_x = paper_x + pad_x;
        let full_content_w = (paper_w - 2.0 * pad_x).max(60.0) as f32;
        let content_y = paper_y + pad_top;
        let content_h = (paper_h - pad_top - pad_bot).max(60.0) as f32;
        // A spread lays each page out in half the width (minus a centre gutter);
        // paged/continuous use the full column. The pagination width must match
        // the column the pages are shown in, so the layout key carries it.
        let gutter = if framed {
            f64::from(sp.s12)
        } else {
            f64::from(sp.s8)
        };
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
            header_y,
            rule_y,
            slider_band,
        }
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
        self.page_top_block = panes.first().map_or(0, |pane| {
            let (ts, te) = (pane.win_start, pane.win_end);
            layout
                .blocks
                .iter()
                .find(|b| b.lines.iter().any(|l| l.y >= ts - 0.5 && l.y < te))
                .map_or(0, |b| b.index)
        });
        panes
    }

    /// The paper background (framed card or borderless fill) and the running
    /// chapter-title header + rule (framed only — a borderless small screen
    /// drops it to hand every pixel to the text; the chapter is still named
    /// in the bottom chrome bar).
    fn draw_reader_page_surface(
        &self,
        ctx: &mut PaintCtx<'_>,
        lib: &[Arc<Book>],
        palette: &ReaderPalette,
        geom: &ReaderPageGeometry,
    ) {
        let theme = ctx.theme;
        let paper = Rect::new(
            geom.paper_x,
            geom.paper_y,
            geom.paper_x + geom.paper_w,
            geom.paper_y + geom.paper_h,
        );
        if geom.framed {
            // The page is content on the window ground: no shadow, its own
            // edge measured from the page colour.
            let radius = f64::from(theme.tokens.radii.control);
            ctx.fill_round(paper, radius, palette.page);
            ctx.stroke_round(paper, radius, palette.page_edge, 1.0);
        } else {
            // Borderless: the page fills the reading area, no edge.
            ctx.fill_rect(paper, palette.page);
        }

        // While text is selected, its controls take the running header's
        // place above the rule.
        if geom.framed && self.sel.is_none_or(|selection| selection.is_empty()) {
            if let Some(book) = lib.get(self.book_idx)
                && let Some(ch) = book.chapters.get(self.chapter)
            {
                // One line, elided to the column: the slot above the rule is
                // a single line box, so a long title must not wrap into it.
                let style = theme.text(TextRole::Caption);
                let head = ctx
                    .text
                    .elide_to_width_style(&ch.title, geom.content_w, &style);
                ctx.text(
                    &head,
                    &style,
                    palette.ink_dim,
                    Point::new(geom.content_x, geom.header_y),
                    None,
                );
            }
            paint::line(
                ctx.scene,
                ctx.transform,
                Point::new(geom.content_x, geom.rule_y),
                Point::new(geom.content_x + f64::from(geom.content_w), geom.rule_y),
                palette.rule,
                f64::from(theme.tokens.metrics.chrome_rule_px),
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
        reduce_motion: bool,
    ) -> (bool, Affine, f32) {
        if reduce_motion {
            self.page_turn = None;
            return (false, t, 1.0);
        }
        let now = ui::time::now();
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
    fn draw_reader_columns(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        frame: &ReaderFrame<'_>,
        panes: &[Pane],
        geom: &ReaderPageGeometry,
        mode: ReadMode,
    ) -> (SelAnchor, CommentBoxBounds) {
        let theme = ctx.theme;
        let mut sel_anchor: SelAnchor = None;
        let mut comment_box: CommentBoxBounds = None;

        let (turning, content_t, turn_alpha) =
            self.page_turn_transform(ctx.transform, mode, geom.paper_w, theme.reduce_motion);
        if turning {
            let clip = Rect::new(
                geom.paper_x + 1.0,
                geom.content_y - 3.0,
                geom.paper_x + geom.paper_w - 1.0,
                geom.content_y + f64::from(geom.content_h) + 3.0,
            );
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                turn_alpha,
                ctx.transform,
                &clip,
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
                ctx.scene
                    .push_layer(Fill::NonZero, BlendMode::default(), 1.0, content_t, &clip);
            }
            let (a, cb) = self.draw_pane(&mut ctx.with_transform(content_t), frame, pane);
            if pane.partial {
                ctx.scene.pop_layer();
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
            ctx.scene.pop_layer();
        }
        (sel_anchor, comment_box)
    }

    /// Draw one reading column: margins, selection/highlight bands (rebuilds
    /// `sel_runs`), and glyphs. Returns the first selected band's screen
    /// `(x, y)` and the open comment overlay's highlight bounds, if any.
    fn draw_pane(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        frame: &ReaderFrame<'_>,
        pane: &Pane,
    ) -> (SelAnchor, CommentBoxBounds) {
        // ---- note bands + bookmark glyphs (behind text) -----------------
        Self::draw_pane_note_bands(ctx, frame, pane);

        // ---- selection + ranged-highlight bands; rebuild sel_runs -------
        let bands = self.draw_pane_selection_bands(ctx, frame, pane);

        // ---- draggable highlight markers (left-margin grab handles) -----
        // One grip per highlight group, at the top of its first band. Recorded
        // in `hi_markers` so the press handler can hit-test it.
        self.draw_pane_highlight_markers(ctx, frame.palette, pane.x, &bands.group_top);

        // ---- glyphs + decorations (on top of bands) ---------------------
        let compact = self.rect.width() < self.reader_compact_breakpoint;
        Self::draw_pane_glyphs(ctx, frame, pane, compact);

        (bands.sel_anchor, bands.comment_box)
    }

    /// Note-comment bands and bookmark glyphs, drawn behind the text.
    fn draw_pane_note_bands(ctx: &mut PaintCtx<'_>, frame: &ReaderFrame<'_>, pane: &Pane) {
        let palette = frame.palette;
        let (content_x, content_w) = (pane.x, pane.w);
        for b in &frame.layout.blocks {
            let visible: Vec<&crate::paginate::LaidLine> = b
                .lines
                .iter()
                .filter(|l| pane.shows(l.y, b.line_height))
                .collect();
            let (Some(first), Some(last)) = (visible.first(), visible.last()) else {
                continue;
            };
            let by0 = pane.screen_y(first.y) - 3.0;
            let by1 = pane.screen_y(last.y) + f64::from(b.line_height);
            let chapter = frame.chapter;
            for a in frame
                .annos
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
                        ctx.fill_round(
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
                        ctx.fill_rect(bar, ReaderPalette::highlight_swatch(a.color));
                        let gx = content_x + f64::from(content_w) + 6.0;
                        Icon::MessageSquare.draw_with_stroke(
                            ctx.scene,
                            ctx.transform,
                            Rect::new(gx, by0, gx + 20.0, by0 + 20.0),
                            palette.ink_dim,
                            1.6,
                        );
                    }
                    AnnoKind::Bookmark => {
                        let gx = content_x + f64::from(content_w) + 6.0;
                        Icon::Bookmark.draw_filled(
                            ctx.scene,
                            ctx.transform,
                            Rect::new(gx, by0 - 2.0, gx + 20.0, by0 + 22.0),
                            palette.accent,
                        );
                    }
                }
            }
        }
    }

    /// Selection + ranged-highlight bands across every block/line of the
    /// pane; also rebuilds `self.sel_runs` (needed for hit-testing) and
    /// `self.page_first_doc`/`page_last_doc`.
    fn draw_pane_selection_bands(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        frame: &ReaderFrame<'_>,
        pane: &Pane,
    ) -> PaneBands {
        let ranged: Vec<&Annotation> = frame
            .annos
            .iter()
            .filter(|a| {
                a.chapter == frame.chapter && a.kind == AnnoKind::Highlight && a.start < a.end
            })
            .collect();
        let mut bands = PaneBands::default();
        for b in &frame.layout.blocks {
            if matches!(b.kind, LayoutKind::Separator) {
                continue;
            }
            let block_ranged: Vec<&Annotation> = ranged
                .iter()
                .copied()
                .filter(|a| a.block == b.index)
                .collect();
            for l in &b.lines {
                if !pane.shows(l.y, b.line_height) {
                    continue;
                }
                let line = PaneLine {
                    pane,
                    block: b,
                    line: l,
                    ranged: &block_ranged,
                };
                self.draw_pane_line_bands(ctx, frame, &line, &mut bands);
            }
        }
        bands
    }

    /// Rebuild `sel_runs` (and `page_first_doc`/`page_last_doc`) for every
    /// run of one line, and measure the portion of each run inside the live
    /// selection / a ranged highlight. Pure measurement — the bands
    /// themselves are drawn by the caller once the full span per highlight
    /// is known.
    fn draw_pane_line_runs(
        &mut self,
        tr: &mut TextRenderer,
        frame: &ReaderFrame<'_>,
        line: &PaneLine<'_>,
    ) -> LineSpans {
        let PaneLine {
            pane,
            block: b,
            line: l,
            ranged: block_ranged,
        } = *line;
        let content_x = pane.x;
        let indent = f64::from(b.indent);
        let ly = line.y();
        let lh = line.height();
        let mut spans = LineSpans {
            sel: None,
            hi: vec![None; block_ranged.len()],
        };
        for run in &l.runs {
            let weight = if run.bold {
                TextWeight::Bold
            } else {
                TextWeight::Regular
            };
            let fam = if run.mono || frame.base_mono {
                FontChoice::Monospace
            } else {
                frame.base_family.clone()
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
            if let Some((s, e)) = frame.sel_ord
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
                    grow(&mut spans.sel, bx0, bx1);
                }
            }
            for (i, a) in block_ranged.iter().enumerate() {
                let lo = a.start.max(run.char_start);
                let hi = a.end.min(run_end);
                if lo < hi {
                    let (bx0, bx1) =
                        run_sub_x(x0, f64::from(w), run.char_start, run.char_len, lo, hi);
                    if let Some(span) = spans.hi.get_mut(i) {
                        grow(span, bx0, bx1);
                    }
                }
            }
        }
        spans
    }

    /// Selection + ranged-highlight bands for one visible line: rebuilds its
    /// `sel_runs` (and `page_first_doc`/`page_last_doc`), then draws the
    /// highlight band(s) and the live-selection band for the line.
    fn draw_pane_line_bands(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        frame: &ReaderFrame<'_>,
        line: &PaneLine<'_>,
        bands: &mut PaneBands,
    ) {
        let palette = frame.palette;
        let ly = line.y();
        let lh = line.height();
        let spans = self.draw_pane_line_runs(ctx.text, frame, line);
        for (i, a) in line.ranged.iter().enumerate() {
            if let Some((mnx, mxx)) = spans.hi.get(i).copied().flatten() {
                let band = Rect::new(mnx - 2.0, ly - 1.0, mxx + 2.0, ly + lh - 2.0);
                ctx.fill_round(band, 4.0, palette.highlight_fill(a.color));
                self.hi_bands.push(band);
                bands
                    .group_top
                    .entry(a.group)
                    .and_modify(|e| {
                        if band.y0 < e.0 {
                            *e = (band.y0, a.color);
                        }
                    })
                    .or_insert((band.y0, a.color));
                if frame.comment_group == Some(a.group) {
                    bands.comment_box = Some(match bands.comment_box {
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
        if let Some((mnx, mxx)) = spans.sel {
            let band = Rect::new(mnx - 2.0, ly - 1.0, mxx + 2.0, ly + lh - 2.0);
            let radius = f64::from(ctx.theme.tokens.radii.detail);
            ctx.fill_round(band, radius, ctx.theme.tokens.colors.intent.selection_tint);
            if bands.sel_anchor.is_none() {
                bands.sel_anchor = Some((f64::midpoint(mnx, mxx), ly));
            }
        }
    }

    /// Draggable highlight-margin markers: one grip per highlight group, at
    /// the top of its first band on this pane.
    fn draw_pane_highlight_markers(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        palette: &ReaderPalette,
        content_x: f64,
        group_top: &HashMap<Uuid, (f64, HiColor)>,
    ) {
        for (group, (gy, color)) in group_top {
            let mx0 = content_x - 15.0;
            let handle = Rect::new(mx0, *gy, mx0 + 7.0, *gy + 22.0);
            let hovered =
                self.marker_group == Some(*group) || handle.inflate(5.0, 5.0).contains(self.cursor);
            ctx.fill_round(handle, 3.5, ReaderPalette::highlight_swatch(*color));
            if hovered {
                ctx.stroke_round(handle.inflate(2.0, 2.0), 4.5, palette.ink_dim, 1.4);
            }
            self.hi_markers.push((handle.inflate(6.0, 5.0), *group));
        }
    }

    /// Block decorations (Quote rule, Separator line, inline math, list
    /// bullets) and every run's glyphs, on top of the bands.
    fn draw_pane_glyphs(
        ctx: &mut PaintCtx<'_>,
        frame: &ReaderFrame<'_>,
        pane: &Pane,
        clip_to_narrow_page: bool,
    ) {
        let palette = frame.palette;
        let (content_x, content_w) = (pane.x, pane.w);
        // The layout engine guarantees line bounds, while this paint clip is
        // the final containment guard for font backends with generous glyph
        // overhang. Leave an allowance for list bullets and quote ornament on
        // the left and for hanging punctuation and overhang on the right, both
        // inside the page's own margin, so no glyph escapes the page.
        if clip_to_narrow_page {
            let margin = f64::from(ui::tokens::spacing::Spacing::BASE.s3);
            let paint_bounds = Rect::new(
                content_x - 2.0 * margin,
                pane.y,
                content_x + f64::from(content_w) + margin,
                pane.y + f64::from(pane.win_end - pane.win_start),
            );
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                ctx.transform,
                &paint_bounds,
            );
        }
        for b in &frame.layout.blocks {
            Self::draw_pane_block_decoration(ctx, palette, pane, b);
            if let Some(math) = &b.math
                && pane.shows(b.top, b.line_height)
            {
                ui::math::draw_layout(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
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
                ctx.text(
                    bullet,
                    &TextStyle::new(b.size),
                    palette.ink_dim,
                    Point::new(content_x + f64::from(b.indent) - 20.0, ly),
                    None,
                );
            }
            Self::draw_pane_block_runs(ctx, frame, pane, b);
        }
        if clip_to_narrow_page {
            ctx.scene.pop_layer();
        }
    }

    /// The Quote side-rule, or the Separator's centred rule.
    fn draw_pane_block_decoration(
        ctx: &mut PaintCtx<'_>,
        palette: &ReaderPalette,
        pane: &Pane,
        b: &crate::paginate::LaidBlock,
    ) {
        let (content_x, content_w) = (pane.x, pane.w);
        match b.kind {
            LayoutKind::Quote | LayoutKind::Verse { quoted: true } => {
                let visible: Vec<&crate::paginate::LaidLine> = b
                    .lines
                    .iter()
                    .filter(|l| pane.shows(l.y, b.line_height))
                    .collect();
                if let (Some(f), Some(la)) = (visible.first(), visible.last()) {
                    let y0 = pane.screen_y(f.y);
                    let y1 = pane.screen_y(la.y) + f64::from(b.line_height);
                    let bar = Rect::new(content_x + 10.0, y0, content_x + 13.5, y1);
                    ctx.fill_rect(bar, palette.rule);
                }
            }
            LayoutKind::Separator if pane.shows(b.top, b.line_height) => {
                let cy = pane.screen_y(b.top) + f64::from(b.line_height) / 2.0;
                let cx = content_x + f64::from(content_w) / 2.0;
                paint::line(
                    ctx.scene,
                    ctx.transform,
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
    fn draw_pane_block_runs(
        ctx: &mut PaintCtx<'_>,
        frame: &ReaderFrame<'_>,
        pane: &Pane,
        b: &crate::paginate::LaidBlock,
    ) {
        let (palette, base_mono) = (frame.palette, frame.base_mono);
        let content_x = pane.x;
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
                    frame.base_family.clone()
                };
                if run.mono && !base_mono && run.math.is_none() {
                    let (cw, _) = ctx.text.measure_font(
                        &run.display_text,
                        b.size,
                        None,
                        weight,
                        run.italic,
                        &FontChoice::Monospace,
                    );
                    let cw = cw * l.glyph_scale;
                    let chip = Rect::new(
                        x - 3.0,
                        run_y - 1.0,
                        x + f64::from(cw) + 3.0,
                        run_y + f64::from(b.size) + 3.0,
                    );
                    ctx.fill_round(chip, 3.0, palette.code_bg);
                }
                if let Some(math) = &run.math {
                    ui::math::draw_layout(
                        ctx.scene,
                        ctx.transform
                            * Affine::translate((x, run_y))
                            * Affine::scale_non_uniform(f64::from(l.glyph_scale), 1.0),
                        ctx.text,
                        math,
                        Point::ZERO,
                        palette.ink,
                    );
                } else {
                    ctx.text.draw_style(
                        ctx.scene,
                        &run.display_text,
                        palette.ink,
                        ctx.transform
                            * Affine::translate((x, run_y))
                            * Affine::scale_non_uniform(f64::from(l.glyph_scale), 1.0),
                        None,
                        &TextStyle::new(b.size)
                            .with_weight(weight)
                            .with_italic(run.italic)
                            .with_family(fam.clone()),
                    );
                }
            }
        }
    }

    /// A floating toolbar shown while a paragraph is selected: highlight
    /// colours + Note, reachable at any width even with the rail collapsed.
    fn draw_selection_hud(&mut self, ctx: &mut PaintCtx<'_>, geom: &ReaderPageGeometry) {
        let theme = ctx.theme;
        // Hide the picker while a note is being composed (the composer takes
        // over) or when there's no active selection/block.
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        if self.view != View::Reader || !has_sel || self.composing.is_some() {
            return;
        }
        // The shared reader HUD owns look/spacing/hover; this reader keeps the
        // page palette's accent and maps emitted actions to its own commands.
        let del_group = self.selection_highlight_group();
        // The HUD is app chrome: it reads the window's theme end to end and
        // the reading palette stays on the page.
        let swatches: Vec<ui::HudSwatch> = HiColor::ALL
            .iter()
            .map(|&col| {
                ui::HudSwatch::new(
                    [col.rgb().0, col.rgb().1, col.rgb().2],
                    self.hi_color == col,
                )
                .named(col.name())
            })
            .collect();
        // Docked in the slot above the prose, centred on the page, never
        // over a line of text; inside the reading column, clear of the rails.
        let anchor_pt = Point::new(geom.paper_x + geom.paper_w / 2.0, geom.content_y);
        let bounds = Rect::new(geom.center_x0, geom.paper_y, geom.center_x1, geom.content_y);
        let hud = ui::SelectionHud::draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ui::HudControls {
                swatches: &swatches,
                show_note: true,
                show_delete: del_group.is_some(),
                show_select_all: false,
                shortcuts: false,
            },
            ui::HudPlacement::new(anchor_pt, bounds),
            self.cursor,
        );

        // Swallow taps on the bar body, and route each control to a command.
        self.push_hit(hud.rect(), Action::Swallow);
        for &(rect, action) in hud.buttons() {
            let mapped = match action {
                ui::HudAction::Copy => Action::CopySelection,
                ui::HudAction::Note => Action::AddNote,
                ui::HudAction::Highlight(i) => Action::SetColor(
                    HiColor::ALL
                        .get(i.min(HiColor::ALL.len() - 1))
                        .copied()
                        .unwrap_or(HiColor::ALL[0]),
                ),
                ui::HudAction::Delete => match del_group {
                    Some(g) => Action::DeleteHighlight(g),
                    None => Action::Swallow,
                },
                ui::HudAction::SelectAll => Action::Swallow,
            };
            self.push_hit(rect, mapped);
        }
    }

    /// The on-page note composer: a small card anchored near the selected
    /// passage. Typing is handled by the composing key path; this draws the
    /// surface, live text with a caret, and Save/Delete.
    fn draw_note_composer(&mut self, ctx: &mut PaintCtx<'_>, anchor_y: Option<f64>) {
        let theme = ctx.theme;
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
        self.note_composer.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            self.rect,
            anchor_y,
            annotation,
        );
    }

    /// The contents sidebar and the annotations inspector: docked regions
    /// beside the page, or floating drawers over it; a press on the page
    /// beside an open drawer dismisses it.
    fn draw_rails(&mut self, ctx: &mut PaintCtx<'_>, lib: &[Arc<Book>]) {
        let theme = ctx.theme;
        if let Some(panel) = self.toc_panel_rect() {
            if self.toc_drawer.is_overlay() {
                self.push_hit(self.toc_drawer.scrim_rect(), Action::DismissOverlay);
                self.push_hit(panel, Action::Swallow);
            }
            let local = self.toc_drawer.paint_panel(ctx.scene, ctx.transform, theme);
            let mut local_ctx = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &local);
            self.draw_toc(&mut local_ctx, lib, panel);
        }
        if let Some(panel) = self.notes_panel_rect() {
            if self.notes_drawer.is_overlay() {
                self.push_hit(self.notes_drawer.scrim_rect(), Action::DismissOverlay);
                self.push_hit(panel, Action::Swallow);
            }
            let local = self
                .notes_drawer
                .paint_panel(ctx.scene, ctx.transform, theme);
            let mut local_ctx = PaintCtx::new(ctx.scene, ctx.transform, ctx.text, &local);
            self.draw_notes(&mut local_ctx, panel);
        }
    }

    /// Whether the contents sidebar shows its own header (and with it the
    /// sidebar toggle): docked and open.
    fn toc_docked_open(&self) -> bool {
        self.toc_drawer.is_docked() && self.toc_drawer.reveal() > 0.5
    }

    /// The content column's header row: the sidebar toggle (when the
    /// sidebar is hidden), Back, the book's title and author on one
    /// baseline, and the reading controls at the trailing edge. No fill; one
    /// hairline under it.
    fn draw_reader_header(&mut self, ctx: &mut PaintCtx<'_>, lib: &[Arc<Book>]) {
        let theme = ctx.theme;
        let content = self.notes_drawer.content_rect();
        let band = Rect::new(
            content.x0,
            self.rect.y0,
            content.x1,
            self.rect.y0 + self.header_h,
        );
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            band.x0,
            band.x1,
            band.y1,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        let spacing = &theme.tokens.spacing;
        let (inset, gap) = (f64::from(spacing.s3), f64::from(spacing.rhythm.related));
        let side = ControlSize::Regular.height(theme);
        let cy = band.center().y;

        let mut x = band.x0 + inset;
        if !self.toc_docked_open() {
            let toggle = Rect::new(x, cy - side / 2.0, x + side, cy + side / 2.0);
            self.toc_drawer.draw_toggle(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                toggle,
                toggle.contains(self.cursor),
            );
            self.push_hit(pointer_target(toggle, theme), Action::ToggleToc);
            x = toggle.x1 + gap;
        }
        let back = self.icon_button(
            ctx,
            Point::new(x + side / 2.0, cy),
            ControlSize::Regular,
            (Icon::ChevronLeft, "Back to library"),
            false,
            Action::Back,
        );
        let title_x0 = back.x1 + gap;

        // Trailing, walking in from the edge: annotations, bookmark, type.
        let mut x1 = band.x1 - inset;
        let notes = Rect::new(x1 - side, cy - side / 2.0, x1, cy + side / 2.0);
        self.notes_drawer.draw_toggle(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            notes,
            notes.contains(self.cursor),
        );
        self.push_hit(pointer_target(notes, theme), Action::ToggleNotes);
        x1 = notes.x0 - gap;
        let bookmark = Rect::new(x1 - side, cy - side / 2.0, x1, cy + side / 2.0);
        self.draw_bookmark_button(ctx, bookmark);
        x1 = bookmark.x0 - gap;
        let type_w = button_width(ctx, "Aa", None, ControlSize::Regular);
        let type_rect = Rect::new(x1 - type_w, cy - side / 2.0, x1, cy + side / 2.0);
        self.button(
            ctx,
            ButtonFace::new(type_rect, "Aa")
                .accessible_name("Reading appearance")
                .role(ButtonRole::Plain)
                .pressed(self.reading_settings.is_open()),
            Action::ToggleTypePopover,
        );
        self.type_button = type_rect;
        let title_x1 = type_rect.x0 - inset;

        if let Some(book) = lib.get(self.book_idx) {
            Self::draw_title_and_detail(ctx, band, (title_x0, title_x1), &book.title, &book.author);
        }
    }

    /// The page bookmark toggle: a plain button with the bookmark glyph,
    /// filled while the page is bookmarked.
    fn draw_bookmark_button(&mut self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let bookmarked = self.page_bookmarked();
        self.button(
            ctx,
            ButtonFace::new(rect, "")
                .accessible_name("Bookmark this page")
                .role(ButtonRole::Plain)
                .pressed(bookmarked),
            Action::ToggleBookmark,
        );
        let glyph = f64::from(theme.tokens.metrics.icon_md);
        let ink = if bookmarked || rect.contains(self.cursor) {
            theme.text_primary
        } else {
            theme.text_dim
        };
        let glyph = Rect::from_center_size(rect.center(), (glyph, glyph));
        if bookmarked {
            Icon::Bookmark.draw_filled(ctx.scene, ctx.transform, glyph, ink);
        } else {
            Icon::Bookmark.draw(ctx.scene, ctx.transform, glyph, ink);
        }
    }

    /// A header's title in `Heading` with a secondary detail (an author, a
    /// count) after it in `Body`, both on one baseline and centred as a line
    /// in `band`, elided to `x0..x1`. The detail is dropped before the title
    /// is shortened.
    fn draw_title_and_detail(
        ctx: &mut PaintCtx<'_>,
        band: Rect,
        (x0, x1): (f64, f64),
        title: &str,
        detail: &str,
    ) {
        let theme = ctx.theme;
        let room = x1 - x0;
        if room <= 0.0 {
            return;
        }
        let title_style = theme.text(TextRole::Heading);
        let detail_style = theme.text(TextRole::Body);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let shown = ctx
            .text
            .elide_to_width_style(title, ui::num::f32_of_f64(room), &title_style);
        let (title_w, title_h) = ctx.text.measure_style(&shown, None, &title_style);
        let (detail_w, _) = ctx.text.measure_style(detail, None, &detail_style);
        let with_detail = !detail.is_empty()
            && shown == title
            && f64::from(title_w) + gap + f64::from(detail_w) <= room;
        let top = ui::text_metrics::centered_text_top(band.y0, band.height(), f64::from(title_h));
        ctx.text(
            &shown,
            &title_style,
            theme.text_primary,
            Point::new(x0, top),
            None,
        );
        if with_detail {
            let baseline =
                top + f64::from(ctx.text.first_baseline_style(&shown, None, &title_style));
            let detail_top =
                baseline - f64::from(ctx.text.first_baseline_style(detail, None, &detail_style));
            ctx.text(
                detail,
                &detail_style,
                theme.text_dim,
                Point::new(x0 + f64::from(title_w) + gap, detail_top),
                None,
            );
        }
    }

    /// The contents sidebar inside `panel`: its header (the sidebar toggle
    /// and "Contents"), then one navigation row per chapter, scrolling, the
    /// current chapter lifted.
    fn draw_toc(&mut self, ctx: &mut PaintCtx<'_>, lib: &[Arc<Book>], panel: Rect) {
        let theme = ctx.theme;
        let spacing = &theme.tokens.spacing;
        let header = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + self.header_h);
        let side = ControlSize::Regular.height(theme);
        let inset = f64::from(spacing.s3);
        let toggle = Rect::new(
            header.x0 + inset,
            header.center().y - side / 2.0,
            header.x0 + inset + side,
            header.center().y + side / 2.0,
        );
        self.toc_drawer.draw_toggle(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            toggle,
            toggle.contains(self.cursor),
        );
        self.push_hit(pointer_target(toggle, theme), Action::ToggleToc);
        let title_x = toggle.x1 + f64::from(spacing.rhythm.related);
        ctx.text_role_in_band(
            "Contents",
            TextRole::Heading,
            theme.text_primary,
            title_x,
            header,
            Some(ui::num::f32_of_f64((header.x1 - inset - title_x).max(1.0))),
        );

        let Some(book) = lib.get(self.book_idx) else {
            return;
        };
        let list = Rect::new(
            panel.x0,
            header.y1,
            panel.x1,
            panel.y1 - f64::from(spacing.s2),
        );
        let row_h = ui::chrome::nav_row_height(theme);
        self.toc_row_pitch = row_h;
        let content_h = book.chapters.len() as f64 * row_h;
        let visible_h = list.height().max(0.0);
        self.toc_max_scroll = (content_h - visible_h).max(0.0);
        if self.toc_scroll_to_active {
            let active_y = self.chapter as f64 * row_h;
            if active_y < self.toc_scroll {
                self.toc_scroll = active_y;
            } else if active_y + row_h > self.toc_scroll + visible_h {
                self.toc_scroll = active_y + row_h - visible_h;
            }
            self.toc_scroll_to_active = false;
        }
        self.toc_scroll = self.toc_scroll.clamp(0.0, self.toc_max_scroll);

        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &list,
        );
        for (i, chapter) in book.chapters.iter().enumerate() {
            let y = list.y0 + i as f64 * row_h - self.toc_scroll;
            if y + row_h < list.y0 || y > list.y1 {
                continue;
            }
            let row = Rect::new(list.x0, y, list.x1, y + row_h);
            let hovered = row.contains(self.cursor) && list.contains(self.cursor);
            let focused = self.semantic_focus.as_deref()
                == Some(format!("books.reader.chapter:{i}").as_str());
            let face = NavRowFace::new(row, &chapter.title)
                .state(SelectionState::new(i == self.chapter, hovered, false))
                .focused(focused);
            ui::chrome::paint_nav_row(ctx.scene, ctx.transform, ctx.text, theme, &face);
            self.push_hit(row.intersect(list), Action::GotoChapter(i));
        }
        ctx.scene.pop_layer();
        paint_scroll_thumb(
            ctx,
            list,
            ScrollMetrics::new(content_h, visible_h, self.toc_scroll),
        );
    }

    /// The annotations inspector inside `panel`: its header ("Annotations"
    /// and how many), how to add one, and one card per annotation, scrolling.
    fn draw_notes(&mut self, ctx: &mut PaintCtx<'_>, panel: Rect) {
        let theme = ctx.theme;
        let spacing = &theme.tokens.spacing;
        let annos = self.annotations.clone();
        let inset = f64::from(spacing.s3);
        let header = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + self.header_h);
        let count = annos.len().to_string();
        Self::draw_title_and_detail(
            ctx,
            header,
            (header.x0 + inset, header.x1 - inset),
            "Annotations",
            &count,
        );
        if self.notes_drawer.is_docked() {
            ui::surface::separator(
                ctx.scene,
                ctx.transform,
                header.x0,
                header.x1,
                header.y1,
                0.0,
                theme.tokens.colors.surfaces.content,
            );
        }

        let (x0, x1) = (panel.x0 + inset, panel.x1 - inset);
        let mut y = header.y1 + inset;
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        if has_sel {
            let height = ControlSize::Small.height(theme);
            let width = button_width(ctx, "Add note", Some(Icon::Plus), ControlSize::Small);
            let rect = Rect::new(x0, y, x0 + width, y + height);
            self.button(
                ctx,
                ButtonFace::new(rect, "Add note")
                    .icon(Icon::Plus)
                    .control_size(ControlSize::Small),
                Action::AddNote,
            );
            y = rect.y1;
        } else {
            let hint = "Select text in the book to highlight it or add a note.";
            let wrap = Some(ui::num::f32_of_f64((x1 - x0).max(1.0)));
            ctx.text_role(
                hint,
                TextRole::Caption,
                theme.text_dim,
                Point::new(x0, y),
                wrap,
            );
            y += ctx.measure_role(hint, TextRole::Caption, wrap).height;
        }

        let gap = f64::from(spacing.rhythm.sibling);
        let list = Rect::new(
            panel.x0,
            y + gap,
            panel.x1,
            panel.y1 - f64::from(spacing.s2),
        );
        let card_w = x1 - x0;
        let heights: Vec<f64> = annos
            .iter()
            .map(|annotation| NoteCard::new(annotation, self.composing).height(ctx, card_w))
            .collect();
        let content_h = heights.iter().sum::<f64>() + gap * annos.len().saturating_sub(1) as f64;
        let visible_h = list.height().max(0.0);
        self.notes_max_scroll = (content_h - visible_h).max(0.0);
        if let Some(target) = self.notes_scroll_to_annotation.take()
            && let Some(index) = annos.iter().position(|annotation| annotation.id == target)
        {
            let top = heights.iter().take(index).sum::<f64>() + gap * index as f64;
            let bottom = top + heights.get(index).copied().unwrap_or(0.0);
            if top < self.notes_scroll {
                self.notes_scroll = top;
            } else if bottom > self.notes_scroll + visible_h {
                self.notes_scroll = bottom - visible_h;
            }
        }
        self.notes_scroll = self.notes_scroll.clamp(0.0, self.notes_max_scroll);

        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &list,
        );
        let mut card_y = list.y0 - self.notes_scroll;
        for (annotation, height) in annos.iter().zip(&heights) {
            if card_y + height >= list.y0 && card_y <= list.y1 {
                let card = Rect::new(x0, card_y, x1, card_y + height);
                self.draw_note_card(ctx, card, annotation, list);
            }
            card_y += height + gap;
        }
        ctx.scene.pop_layer();
        paint_scroll_thumb(
            ctx,
            list,
            ScrollMetrics::new(content_h, visible_h, self.notes_scroll),
        );
    }

    /// One annotation as a card: what it is and where (a chip and the
    /// chapter and paragraph), its delete control, the passage, and the
    /// note. The card reveals the passage; a note's text opens it for
    /// editing.
    fn draw_note_card(&mut self, ctx: &mut PaintCtx<'_>, card: Rect, a: &Annotation, list: Rect) {
        let theme = ctx.theme;
        let note = NoteCard::new(a, self.composing);
        let hovered = card.contains(self.cursor) && list.contains(self.cursor);
        let face = CardFace::new().pressable().interaction(Interaction {
            hovered,
            focused: note.composing,
            ..Interaction::default()
        });
        face.paint(ctx, card);
        let body = face.body_rect(ctx.text, theme, card);
        let layout = note.layout(ctx, body);

        let lead = match a.kind {
            AnnoKind::Bookmark => ChipLead::None,
            _ => ChipLead::Dot(ReaderPalette::highlight_swatch(a.color)),
        };
        ChipFace::new(layout.chip, a.kind.label()).lead(lead).paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
        let caption = theme.text(TextRole::Caption);
        let locator = ctx.text.elide_to_width_style(
            &note.locator,
            ui::num::f32_of_f64(body.width().max(1.0)),
            &caption,
        );
        ctx.text_role(
            &locator,
            TextRole::Caption,
            theme.text_dim,
            Point::new(body.x0, layout.locator_y),
            None,
        );
        let wrap = Some(ui::num::f32_of_f64(body.width().max(1.0)));
        ctx.text(
            &note.excerpt,
            &NoteCard::excerpt_style(theme),
            theme.text_dim,
            Point::new(body.x0, layout.excerpt_y),
            wrap,
        );
        if !note.text.is_empty() {
            ctx.text_role(
                &note.text,
                TextRole::Body,
                theme.text_primary,
                Point::new(body.x0, layout.note_y),
                wrap,
            );
        }

        // The card reveals the passage; a note's own text edits it. Delete
        // is pushed last so it wins inside the card.
        self.push_hit(card.intersect(list), Action::JumpTo(a.id));
        if a.kind == AnnoKind::Note {
            let text = Rect::new(body.x0, layout.excerpt_y, body.x1, body.y1);
            self.push_hit(text.intersect(list), Action::EditNote(a.id));
        }
        if layout.delete.intersect(list).height() > 0.0 {
            self.button(
                ctx,
                ButtonFace::icon_only(layout.delete, Icon::Trash2, "Delete annotation")
                    .control_size(ControlSize::Small),
                Action::Delete(a.id),
            );
        }
    }
}

/// An annotation's card text: the passage it marks, the note (with a caret
/// while it is being written), and where it is. The list sizes each card
/// from the same measurement it paints.
struct NoteCard {
    kind: &'static str,
    /// The kind chip leads with the highlight's colour (not for bookmarks).
    dot: bool,
    excerpt: String,
    text: String,
    locator: String,
    composing: bool,
}

/// Where a card's parts land inside its body.
struct NoteCardLayout {
    chip: Rect,
    delete: Rect,
    /// Where it is, on the line under the chip.
    locator_y: f64,
    excerpt_y: f64,
    note_y: f64,
}

impl NoteCard {
    fn new(a: &Annotation, composing: Option<Uuid>) -> Self {
        let composing = composing == Some(a.id);
        let excerpt = if a.excerpt.is_empty() {
            "No passage".to_string()
        } else {
            format!("\u{201C}{}\u{201D}", a.excerpt)
        };
        let text = if composing {
            format!("{}\u{2502}", a.note)
        } else {
            a.note.clone()
        };
        Self {
            kind: a.kind.label(),
            dot: a.kind != AnnoKind::Bookmark,
            excerpt,
            text,
            locator: format!("Chapter {}, paragraph {}", a.chapter + 1, a.block + 1),
            composing,
        }
    }

    fn excerpt_style(theme: &Theme) -> TextStyle {
        theme.text(TextRole::Body).with_italic(true)
    }

    /// The header line (chip and delete control) and the y of each text
    /// block inside `body`.
    fn layout(&self, ctx: &mut PaintCtx<'_>, body: Rect) -> NoteCardLayout {
        let theme = ctx.theme;
        let line = ChipFace::height(theme).max(ControlSize::Small.height(theme));
        let lead = if self.dot {
            ChipLead::Dot(theme.text_dim)
        } else {
            ChipLead::None
        };
        let chip_w = ChipFace::intrinsic_width(self.kind, lead, false, ctx.text, theme);
        let chip_h = ChipFace::height(theme);
        let chip = Rect::new(
            body.x0,
            body.y0 + (line - chip_h) / 2.0,
            body.x0 + chip_w,
            body.y0 + f64::midpoint(line, chip_h),
        );
        let side = ControlSize::Small.height(theme);
        let delete = Rect::new(
            body.x1 - side,
            body.y0 + (line - side) / 2.0,
            body.x1,
            body.y0 + f64::midpoint(line, side),
        );
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let wrap = Some(ui::num::f32_of_f64(body.width().max(1.0)));
        let locator_y = body.y0 + line + f64::from(theme.tokens.spacing.rhythm.tight);
        let excerpt_y = locator_y + ctx.line_box(TextRole::Caption) + gap;
        let excerpt_h = f64::from(
            ctx.text
                .measure_style(&self.excerpt, wrap, &Self::excerpt_style(theme))
                .1,
        );
        NoteCardLayout {
            chip,
            delete,
            locator_y,
            excerpt_y,
            note_y: excerpt_y + excerpt_h + gap,
        }
    }

    /// The card's full height at `width`.
    fn height(&self, ctx: &mut PaintCtx<'_>, width: f64) -> f64 {
        let theme = ctx.theme;
        let face = CardFace::new();
        let probe = Rect::new(0.0, 0.0, width, 1.0e6);
        let body = face.body_rect(ctx.text, theme, probe);
        let layout = self.layout(ctx, body);
        let bottom = if self.text.is_empty() {
            layout.note_y - f64::from(theme.tokens.spacing.rhythm.related)
        } else {
            let wrap = Some(ui::num::f32_of_f64(body.width().max(1.0)));
            layout.note_y + ctx.measure_role(&self.text, TextRole::Body, wrap).height
        };
        face.height_for_body(ctx.text, theme, width, bottom - body.y0)
    }
}

/// The legend printed on `key`'s cap in the active keyboard layout, as a
/// shortcut row and its messages name it.
fn key_legend(key: &str) -> String {
    ui::KeyLegend::from_name(key).label(ui::PlatformConventions::default())
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

/// How far through the book the reader is, as the footer shows it.
fn progress_label(fraction: f32) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as u8)
}

/// The width the widest progress label takes, so the slider beside it
/// keeps one length as the number grows.
fn progress_label_width(ctx: &mut PaintCtx<'_>) -> f64 {
    let style = ctx.theme.text(TextRole::Caption).numeric();
    f64::from(ctx.text.measure_style("100%", None, &style).0)
}

/// The progress label in `rect`, centred on its height.
fn draw_progress_label(ctx: &mut PaintCtx<'_>, rect: Rect, fraction: f32, color: Color) {
    let label = progress_label(fraction);
    let style = ctx.theme.text(TextRole::Caption).numeric();
    let height = f64::from(ctx.text.measure_style(&label, None, &style).1);
    ctx.text(
        &label,
        &style,
        color,
        Point::new(rect.x0, rect.center().y - height / 2.0),
        Some(rect.width() as f32),
    );
}

mod book_card;
use book_card::ShelfCard;
mod catalogue_settings;
mod chapter_layouts;
mod query_view;
mod search_worker;
mod settings_page;
use settings_page::{SettingsColumn, SettingsRow};
mod stage;
use search_worker::SearchWorker;

pub(crate) use query_view::PASSAGE;
pub(crate) use query_view::kinds as query_kinds;

#[cfg(test)]
#[path = "app/interaction_tests.rs"]
mod interaction_tests;

#[cfg(test)]
#[path = "app/query_view_tests.rs"]
mod query_view_tests;

#[cfg(test)]
#[path = "app/shelf_tests.rs"]
mod shelf_tests;

#[cfg(test)]
#[path = "app/scroll_tests.rs"]
mod scroll_tests;
