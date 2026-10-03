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
//! State: `State::doc` (CRDT) holds settings/progress/annotations, mutated
//! through typed replayable actions. Ephemeral widget caches (open book,
//! page index, selection, layout cache) are per-peer view state kept out of
//! the document so readers browse independently.

#![allow(
    clippy::many_single_char_names,
    reason = "t (Affine), c (Colors), r (Rect), x/y/w/h (geometry) and their close relatives \
              are the standard vocabulary shared by every draw/hit-test helper in this file"
)]

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_clock::unix_now_ms;
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{
    BlendMode, Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData as PenikoImage,
    ImageFormat as ImageFmt,
};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::time::{Duration, Instant};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::animation::{Animated, Curve};
use ui::chrome::SelectionState;
use ui::gesture::{DragSelect, Granularity};
use ui::paint::PaintCtx;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
};
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::metrics::Metrics;
use ui::widget::{
    Button, ButtonFace, Continuable, EditOutcome, EditableLabel, FlapState, Slider, SliderFace,
    SplitLayout, Toggle, ToggleFace, Widget,
};

use crate::chrome::{self as chrome, pointer_target, rhythm};
use crate::comment_overlay::{CommentAction, CommentOverlay};
use crate::content::{self, is_shared};
use crate::doc::BooksDoc;
use crate::input_controls::{
    ColorEdit, ColorSheetResponse, InputEventCtx, NoteAction, NoteComposer, NoteSemanticOutcome,
    NumberControls, NumberField, PickTarget, SchemeColorSheet,
};
use crate::local_library::LocalLibrary;
use crate::model::{
    AnnoKind, Annotation, AnnotationAnchor, Book, ColorScheme, CompositionStyle, FontEntry,
    FontKind, HiColor, Progress, ReadMode, ReadTheme, Settings, default_fonts, default_schemes,
};
use crate::paginate::{
    ChapterLayout, ChapterLayoutRequest, LayoutKey, LayoutKind, font_choice, layout_chapter,
};
use crate::paint;
use crate::reader_bubble::{ReaderBubble, ReaderBubbleAction, ReaderPosition};
use crate::reader_theme::ReaderPalette;
use crate::reading_settings::{Appearance, ReadingSettings, ReadingSettingsAction};
use crate::shelf_controls::{ShelfControlAction, ShelfControls};

const TOP_BAR: f64 = 54.0;
/// Side of the square −/+ buttons in a size stepper.
const SIZE_STEPPER_BTN: f64 = 28.0;
/// Width of the quick-settings visibility toggle in a font row.
const FONT_ROW_TOGGLE_W: f64 = 40.0;

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

/// A completed immutable search projection. The worker owns all O(n) index
/// construction; the UI thread only installs this already-complete value.
struct SearchBuild {
    fingerprint: u64,
    generation: u64,
    provider: crate::search_provider::BookSearchProvider,
}

struct SearchWorker {
    results: mpsc::Receiver<SearchBuild>,
}

impl SearchWorker {
    /// `prior` is the published provider; its chapter-prose index is reused
    /// when the book set has not changed.
    fn spawn(
        prior: crate::search_provider::BookSearchProvider,
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<crate::model::LibraryEntry>>,
        fingerprint: u64,
        generation: u64,
    ) -> Result<Self, String> {
        let (send, results) = mpsc::sync_channel(1);
        ui::rt::spawn_thread("ebook-search-builder", move || {
            let provider = crate::search_provider::BookSearchProvider::new_shared_after(
                &prior,
                books,
                annotations,
                registry,
                generation,
            );
            // The receiver is intentionally allowed to disappear during
            // shutdown; publication has no side effect outside the app.
            let _ = send.send(SearchBuild {
                fingerprint,
                generation,
                provider,
            });
        })
        .map_err(|error| error.to_string())?;
        Ok(Self { results })
    }

    fn try_recv(&self) -> Option<SearchBuild> {
        self.results.try_recv().ok()
    }
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
                    let mut state = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                let (path, result) = match content::parse_shared_path_detailed(&request.path) {
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
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    #[cfg(test)]
    fn close_for_test(&self) {
        let (lock, wake) = &*self.mailbox;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.pending = None;
        drop(state);
        wake.notify_one();
    }
}

impl Drop for ImportWorker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.mailbox;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.pending = None;
        drop(state);
        wake.notify_one();
    }
}

struct LoadingImport {
    generation: u64,
    path: PathBuf,
    kind: ImportKind,
    expected_book_id: Option<String>,
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
    Open,
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

/// Pixels one wheel-line notch maps to, matching the toolkit's other scroll
/// surfaces so a mouse wheel feels consistent across apps.
const WHEEL_LINE_PX: f64 = 48.0;
/// Accumulated reader wheel travel (px) that commits to one page turn.
const TURN_STEP_PX: f64 = 40.0;
/// Accumulated horizontal swipe travel (px) that commits to one page turn;
/// larger than the vertical wheel step so a swipe reads as a deliberate flick.
const SWIPE_STEP_PX: f64 = 80.0;
const TOC_W: f64 = 244.0;
/// Height of the "+ Note on selection" row at the top of the annotations
/// rail: a row that is pressed, so it is the pointer-target metric tall.
const NOTES_ADD_ROW_H: f64 = Metrics::BASE.min_pointer_target as f64;
/// One keyboard-shortcut row in Settings: its binding pill (a pointer target)
/// with a sibling step above and below it.
const SETTINGS_SHORTCUT_ROW_H: f64 =
    Metrics::BASE.min_pointer_target as f64 + 2.0 * rhythm::SIBLING;
use crate::chrome::MENU_ROW_H;
/// Side of the delete control painted on an annotation card: the large icon
/// box. Its hit rect grows to the pointer metric (`chrome::pointer_target`).
const NOTE_CARD_CONTROL: f64 = Metrics::BASE.icon_lg as f64;
/// The annotation card's header band (colour dot, kind, locator, delete):
/// the control with a tight step above and below it.
const NOTE_CARD_HEADER_H: f64 = NOTE_CARD_CONTROL + 2.0 * rhythm::TIGHT;
/// The renderer's nominal single-line box is `size × 1.25`; only the value the
/// running-header slot starts from before the first frame has measured the
/// real line box (`BooksApp::running_header_h`).
const LINE_BOX_FACTOR: f64 = 1.25;

/// Nominal size of the framed page's running header (the renderer scales it);
/// measured in `draw_reader`, painted in `draw_reader_page_surface`.
fn running_header_px() -> f32 {
    11.0
}

/// Contents rail rows: the pill floor and the gap between pills, from the
/// panel-row metric — a one-line row keeps the panel row pitch, and a row
/// grows with its measured (wrapped) title so at 200 % a two-line title gets
/// a two-line row instead of running into the next chapter.
fn toc_row_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1)
}
fn toc_row_floor(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.panel_row_height) - toc_row_gap(theme)
}
const NOTES_W: f64 = 326.0;
const PAGE_MAX_W: f64 = 680.0;
/// The rails are independent reading lists.  Keep their scroll state separate
/// from the page so a wheel over a long table of contents never turns the
/// book underneath it.
const RAIL_SCROLLBAR_W: f64 = 4.0;

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
    /// Device-local reader key bindings.
    Shortcuts,
}

/// Reader commands whose single-character bindings can be tailored per
/// device. Navigation keys (arrows/PageUp/PageDown) always remain available,
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
    /// A downward chevron (dropdown affordance).
    ChevDown,
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

    pub(crate) fn resolve_search_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        ui::search::SearchProvider::resolve_action_resource(&self.search_provider, address)
    }

    pub(crate) fn current_book_resource_context(&self) -> Option<crate::actions::BookResourceCtx> {
        let book = self.open_book()?;
        let block = self.page_top_block;
        Some(crate::actions::BookResourceCtx {
            book_id: book.id.clone(),
            chapter: self.chapter,
            bookmark_block: block,
            bookmark: self.bookmark_plan(block),
            selection_highlight: self
                .sel
                .and_then(|selection| self.resolve_highlight_plan(selection, HiColor::Yellow)),
            selection_note: self.selection_note_draft(String::new()),
            annotations: self.annotations.clone(),
        })
    }

    /// A note on the live selection, anchored where the toolbar's Note button
    /// anchors it: the selected span of its first block.
    fn selection_note_draft(&self, text: String) -> Option<crate::actions::NoteDraft> {
        let (start, end) = self.sel.filter(|s| !s.is_empty())?.ordered();
        let end_ch = if end.block == start.block {
            end.ch.max(start.ch)
        } else {
            self.layout
                .as_ref()
                .and_then(|layout| layout.blocks.iter().find(|b| b.index == start.block))
                .map_or(start.ch, |block| block.char_len)
        };
        Some(crate::actions::NoteDraft {
            book_id: self.book_id(),
            id: Uuid::new_v4(),
            anchor: AnnotationAnchor {
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
        let chars = |block: usize| blocks.get(block).map(|b| b.plain_text().chars().count());
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
        // cleared `last_persisted` would re-adopt the stored chapter instead.
        if self.book_idx != index || self.chapter != chapter {
            self.book_idx = index;
            self.chapter = chapter;
            self.page = 0;
            self.layout = None;
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
              open/closed toggles, and a handful of --shots-only seams); \
              grouping them into enums would not express any real invariant \
              between them and would touch reader, library and settings call \
              sites across this file"
)]
pub struct BooksApp {
    // --- parsed content (render cache, regenerable; not in the CRDT) -------
    // Membership snapshots share each immutable parsed book independently.
    // Adding/removing one shelf entry must never clone another EPUB's chapters.
    library: Arc<Vec<Arc<Book>>>,
    /// Where each book's `.epub` lives, keyed by content id. The durable
    /// record is the CRDT library ([`BooksDoc`]); this is its cache.
    library_paths: HashMap<String, PathBuf>,
    /// Whether the persisted library has been hydrated from the document yet
    /// (loads on the first `sync`; shot/test builds pass one explicitly).
    library_loaded: bool,
    /// A book handed to the app by the OS (file association) to open + import
    /// on the first `sync`.
    pending_open_path: Option<PathBuf>,
    /// A shelf book, by content id, to open on the first `sync`.
    pending_open_book: Option<String>,
    /// A book the host handed over as bytes, to shelve and open on the
    /// first `sync`.
    pending_handed: Option<content::HandedBook>,
    /// The host's content store, where rows naming a content hash find
    /// their bytes on a device without the file.
    content: Option<tilo_content::ContentStore>,
    /// An in-flight host file-picker request, drained once per frame in
    /// `sync`. `None` when no picker is open (the app never draws its own).
    portal_rx: Option<PortalChannel>,
    /// A chooser delivering contents, where files have no path (the web).
    portal_contents_rx: Option<ContentsChannel>,
    /// A book chosen by contents, waiting for the document in `sync`.
    pending_contents: Option<(String, Vec<u8>)>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_lifecycle: Option<ResourceLifecycle>,
    completed_resource_invocation: Option<tilo_actions::InvocationUpdate>,
    /// The sole background parser. It owns no document state; completed books
    /// are committed only by [`Self::drain_import_worker`] on the UI thread.
    import_worker: Option<ImportWorker>,
    /// Spawn failures are deferred into an honest error only if the reader
    /// actually asks to import, rather than failing app startup outright.
    import_worker_error: Option<String>,
    import_generation: u64,
    import_loading: Option<LoadingImport>,
    pending_share: Option<PendingShare>,
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
    /// Host accessibility text scale. Responsive breakpoints use effective
    /// readable width so enlarged type receives uncrowded overlay rails.
    text_scale: f32,

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
    search_provider: crate::search_provider::BookSearchProvider,
    /// Small live action surface (current reader controls plus settings
    /// resources) merged into the compact immutable action-label provider.
    action_search_resources: Arc<Vec<tilo_actions::Resource>>,
    action_search_epoch: Arc<AtomicU64>,
    action_search_fingerprint: u64,
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
    default_font_menu: bool,
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
    last_persisted: Option<(usize, usize, u16)>,
    reading_progress: f32,
    progress_slider_track: Rect,
    progress_dragging: bool,
    pending_seek_fraction: Option<f32>,
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
    /// A `--shots` scenario with a dense annotation history. Kept as a
    /// one-frame seed so live readers never receive fabricated annotations.
    shot_long_annotations: bool,
    /// A visual-a11y proof can intentionally suppress seeded/persisted
    /// annotation decoration, leaving a normal readable page at its target
    /// text scale. This never affects the shared document or live reader.
    shot_clean_reader: bool,
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
    actions: crate::resource::ActionSink,
}

impl BooksApp {
    /// The live app: starts on an empty shelf and hydrates the reader's
    /// persisted library from the document on the first `sync`.
    pub fn new() -> Self {
        let mut app = Self::with_library(Vec::new());
        app.library_loaded = false;
        app.view = View::Library;
        // Where files have no path, the book last opened by contents.
        if !crate::web_file::native_paths()
            && let Some(file) = crate::web_file::recall()
        {
            app.pending_contents = Some((file.name, file.bytes));
        }
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
        let library: Arc<Vec<Arc<Book>>> = Arc::new(library.into_iter().map(Arc::new).collect());
        let search_provider = crate::search_provider::BookSearchProvider::new_shared(
            Arc::clone(&library),
            Arc::new(Vec::new()),
            Arc::new(Vec::new()),
            1,
        );
        Self {
            library,
            view,
            search_provider,
            search_revision: 1,
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
        let (import_worker, import_worker_error) = match ImportWorker::new() {
            Ok(worker) => (Some(worker), None),
            Err(error) => (None, Some(error)),
        };
        let local_library = LocalLibrary::load();
        let watched_folders = local_library.watched_folders.clone();
        Self {
            library: Arc::new(Vec::new()),
            library_paths: HashMap::new(),
            // A caller-supplied library (tests / shots) is authoritative; the live
            // `new()` clears this so the document load runs.
            library_loaded: true,
            pending_open_path: None,
            pending_open_book: None,
            pending_handed: None,
            content: None,
            portal_rx: None,
            portal_contents_rx: None,
            pending_contents: None,
            resource_invocation: None,
            resource_lifecycle: None,
            completed_resource_invocation: None,
            import_worker,
            import_worker_error,
            import_generation: 0,
            import_loading: None,
            pending_share: None,
            load_error: None,
            rect: Rect::ZERO,
            dirty: true,
            view: View::Library,
            book_idx: 0,
            chapter: 0,
            page: 0,
            toc_flap: FlapState::open_when_docked(),
            notes_flap: FlapState::open_when_docked(),
            toc_scroll: 0.0,
            toc_max_scroll: 0.0,
            toc_scroll_to_active: true,
            toc_row_pitch: f64::from(ui::tokens::Metrics::BASE.panel_row_height),
            running_header_h: f64::from(running_header_px()) * LINE_BOX_FACTOR,
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
            text_scale: 1.0,
            hi_markers: Vec::new(),
            marker_drag: None,
            marker_group: None,
            settings: Settings::default(),
            fonts: default_fonts(),
            default_font_id: String::new(),
            schemes: default_schemes(),
            annotations: Vec::new(),
            all_annotations: Arc::new(Vec::new()),
            search_provider: crate::search_provider::BookSearchProvider::new(&[], &[]),
            action_search_resources: Arc::new(Vec::new()),
            action_search_epoch: Arc::new(AtomicU64::new(1)),
            action_search_fingerprint: 0,
            search_worker: None,
            search_fingerprint: 0,
            search_revision: 0,
            settings_tab: SettingsTab::Fonts,
            renaming: None,
            number_controls: NumberControls::default(),
            color_picker: SchemeColorSheet::default(),
            default_font_menu: false,
            library_prefs: crate::model::LibraryPrefs::default(),
            watched_folders,
            watched_sources: HashMap::new(),
            local_library,
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
            reading_progress: 0.0,
            progress_slider_track: Rect::ZERO,
            progress_dragging: false,
            pending_seek_fraction: None,
            cur_page_count: 1,
            seeded: false,
            demo: false,
            shot_settings: None,
            shot_fonts: None,
            shot_schemes: None,
            shot_watched: None,
            shot_compose: false,
            shot_comments: false,
            shot_long_annotations: false,
            shot_clean_reader: false,
            settled_toc: None,
            settled_notes: None,
            layout: None,
            cover_cache: HashMap::new(),
            hits: Vec::new(),
            actions: crate::resource::ActionSink::default(),
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

    /// Open the shelf book with content id `book_id` in the reader, at its
    /// recorded position, once the library loads. The shelf stays home when
    /// the library does not hold it.
    pub fn open_book_on_load(&mut self, book_id: &str) {
        self.pending_open_book = Some(book_id.into());
    }

    /// Read library books from the host's content store as well as files.
    pub fn set_content_store(&mut self, store: tilo_content::ContentStore) {
        self.content = Some(store);
    }

    /// Shelve a book the host handed over and open it once the library
    /// loads, through the same library action an import records.
    pub fn open_handed_on_load(&mut self, handed: content::HandedBook) {
        self.pending_handed = Some(handed);
        self.view = View::Reader;
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
        SplitLayout::from_width(
            self.rect.width() / f64::from(self.text_scale.max(1.0)),
            TOC_BREAKPOINT,
        )
    }

    fn notes_layout(&self) -> SplitLayout {
        SplitLayout::from_width(
            self.rect.width() / f64::from(self.text_scale.max(1.0)),
            NOTES_BREAKPOINT,
        )
    }

    fn toc_width(&self) -> f64 {
        (TOC_W * f64::from(self.text_scale.clamp(1.0, 1.6)))
            .min((self.rect.width() * 0.90).max(TOC_W))
    }

    fn notes_width(&self) -> f64 {
        (NOTES_W * f64::from(self.text_scale.clamp(1.0, 1.45)))
            .min((self.rect.width() * 0.94).max(NOTES_W))
    }

    fn toc_open(&self) -> bool {
        self.toc_flap.is_open(self.toc_layout())
    }

    fn notes_open(&self) -> bool {
        self.notes_flap.is_open(self.notes_layout())
    }

    /// The visible, interactive portion of a rail in this frame.  Overlay
    /// rails use their animated position so a wheel during the slide still
    /// belongs to the rail rather than leaking through to page turns.
    fn toc_panel_rect(&self) -> Option<Rect> {
        if self.toc_flap.reveal() <= 0.001 {
            return None;
        }
        let r = self.rect;
        let width = self.toc_width();
        let x0 = match self.toc_layout() {
            SplitLayout::Docked => r.x0,
            SplitLayout::Overlay => r.x0 - width * (1.0 - self.toc_flap.slide()),
        };
        Some(Rect::new(x0, r.y0 + TOP_BAR, x0 + width, r.y1))
    }

    fn notes_panel_rect(&self) -> Option<Rect> {
        if self.notes_flap.reveal() <= 0.001 {
            return None;
        }
        let r = self.rect;
        let width = self.notes_width();
        let x0 = match self.notes_layout() {
            SplitLayout::Docked => r.x1 - width,
            SplitLayout::Overlay => r.x1 - width * self.notes_flap.slide(),
        };
        Some(Rect::new(x0, r.y0 + TOP_BAR, x0 + width, r.y1))
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
        if self
            .notes_panel_rect()
            .is_some_and(|rect| rect.contains(self.cursor))
        {
            return Some(scroll(&mut self.notes_scroll, self.notes_max_scroll));
        }
        if self
            .toc_panel_rect()
            .is_some_and(|rect| rect.contains(self.cursor))
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
        self.library = Arc::new(
            crate::content::load_sample_library()
                .into_iter()
                .map(Arc::new)
                .collect(),
        );
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
                    content::workspace_books_dir().join("shared.epub"),
                );
            }
            "narrow" => {
                self.view = View::Reader;
                // Contents rail open as an overlay flap; annotations closed so
                // the scrimmed flap reads clearly against the page.
                self.toc_flap.snap(true);
                self.notes_flap.snap(false);
            }
            "import-error" => {
                self.view = View::Reader;
                // Exercise the same persistent, visible error surface used by
                // a real failed picker/import without fabricating a book into
                // the live library.
                self.load_error = Some(
                    "Couldn’t open damaged.epub: epub text is too large (maximum 64 MiB)".into(),
                );
            }
            "import-loading" => {
                self.view = View::Reader;
                self.import_loading = Some(LoadingImport {
                    generation: 1,
                    path: PathBuf::from("/home/reader/Books/Illustrated Atlas.epub"),
                    kind: ImportKind::Open,
                    expected_book_id: None,
                });
            }
            "type" => {
                self.view = View::Reader;
                self.reading_settings.open();
            }
            "library-scrolled" => {
                // The large value clamps to the real max in draw.
                self.view = View::Library;
                self.shelf_scroll = 5000.0;
            }
            "long-toc" => {
                self.view = View::Reader;
                self.toc_flap.snap(true);
                self.notes_flap.snap(false);
                let template = self
                    .library
                    .first()
                    .and_then(|book| book.chapters.first())
                    .cloned();
                if let Some(template) = template
                    && let Some(book) = Arc::make_mut(&mut self.library).first_mut()
                {
                    Arc::make_mut(book).chapters = (0..32)
                        .map(|index| crate::model::Chapter {
                            title: format!("Chapter {} — a longer voyage", index + 1),
                            href: format!("chapter-{}.xhtml", index + 1),
                            blocks: template.blocks.clone(),
                        })
                        .collect();
                    self.chapter = 27;
                    // Keep the staged chapter from being replaced by the
                    // sample book's persisted position during first sync.
                    self.last_persisted = Some((self.chapter, self.page, 0));
                    self.toc_scroll_to_active = true;
                }
            }
            "long-annotations" => {
                self.demo = false;
                self.view = View::Reader;
                self.shot_long_annotations = true;
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
            "large-text" => {
                // A true 200% text scale is a first-class visual/a11y
                // regression state. It must use the normal settings path so
                // pagination and all responsive chrome are exercised too. Do
                // not seed demo annotations here: the proof is a normal,
                // readable page rather than one giant highlighted range.
                self.view = View::Reader;
                self.demo = false;
                self.sel = None;
                self.selected_block = None;
                self.shot_clean_reader = true;
                self.shot_settings = Some(Settings {
                    font_scale: Settings::FONT_MAX,
                    ..Settings::default()
                });
            }
            "reduced-motion" => {
                // The headless renderer has no preference service, so stage
                // the same app-owned motion policy the live host supplies.
                // The still is intentionally calm, but retains the same
                // desktop reader chrome as the normal and high-contrast
                // references so it proves equivalent layout rather than an
                // immersive fallback.
                self.view = View::Reader;
                self.reduce_motion = true;
                self.page_turn = None;
                self.toc_flap.snap(true);
                self.notes_flap.snap(true);
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
            "settings-shortcuts" => {
                self.view = View::Settings;
                self.settings_tab = SettingsTab::Shortcuts;
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
                    opened: unix_now_ms() - (i as u64) * 3_600_000,
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
        // A resource/remote document action can tombstone a shelf row without
        // passing through this window's hover control. Reconcile that durable
        // membership state before deriving reader and shelf caches.
        let removed_ids: Vec<String> = self
            .library
            .iter()
            .filter(|book| BooksDoc::library_is_removed(doc, &book.id))
            .map(|book| book.id.clone())
            .collect();
        if !removed_ids.is_empty() {
            Arc::make_mut(&mut self.library)
                .retain(|book| !removed_ids.iter().any(|id| id == &book.id));
            self.library_paths
                .retain(|id, _| !removed_ids.iter().any(|removed| removed == id));
            self.watched_sources
                .retain(|_, id| !removed_ids.iter().any(|removed| removed == id));
            self.book_idx = self.book_idx.min(self.library.len().saturating_sub(1));
            if self.library.is_empty() {
                self.view = View::Library;
            }
            self.layout = None;
        }
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
        // Keep the immutable search snapshot stable across ordinary frames.
        // Replacing this Arc only for a real CRDT change avoids allocating a
        // second annotation corpus while an unchanged provider is retained.
        if self.all_annotations.as_ref() != &all {
            self.all_annotations = Arc::new(all);
        }
    }

    /// Persist the live reading position for the open book (deduped so we don't
    /// churn the document when nothing moved).
    fn persist_progress(&mut self, doc: &mut AutoCommit) {
        if self.view != View::Reader {
            return;
        }
        let fraction = self.current_progress_fraction();
        let key = (
            self.chapter,
            self.page,
            (fraction * 10_000.0).round() as u16,
        );
        if self.last_persisted == Some(key) {
            return;
        }
        let Some(book) = self.library.get(self.book_idx) else {
            return;
        };
        let id = book.id.clone();
        self.perform_action(
            doc,
            &crate::actions::set_progress_action(
                &id,
                Progress {
                    chapter: self.chapter,
                    page: self.page,
                    fraction,
                    // 0 ⇒ the action preserves the recorded open time.
                    opened: 0,
                },
            ),
        );
        self.last_persisted = Some(key);
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
        self.layout = None;
        self.toc_scroll_to_active = true;
        self.reading_progress = fraction;
    }

    fn resolve_pending_seek_fraction(
        &mut self,
        layout: &ChapterLayout,
        mode: ReadMode,
        geom: &ReaderPageGeometry,
    ) {
        let Some(within) = self.pending_seek_fraction.take() else {
            return;
        };
        if mode.is_continuous() {
            self.scroll_y = (f64::from(layout.total_height) - f64::from(geom.content_h)).max(0.0)
                * f64::from(within);
        } else {
            let count = layout.page_count().max(1);
            let page = ((within * count as f32).floor() as usize).min(count - 1);
            self.page = page - page % mode.page_stride();
        }
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

        // A handed-over book joins the same bounded worker as a picker result;
        // startup never parses an arbitrary file on the first UI frame.
        let pending_open = self.pending_open_path.take();

        Arc::make_mut(&mut self.library).clear();
        self.library_paths.clear();
        for lb in content::load_library_with_content(
            doc,
            &self.watched_folders,
            &self.local_library.books,
            self.content.as_ref(),
        ) {
            self.library_paths.insert(lb.book.id.clone(), lb.path);
            Arc::make_mut(&mut self.library).push(Arc::new(lb.book));
        }

        let pending_book = self
            .pending_open_book
            .take()
            .and_then(|id| self.library.iter().position(|book| book.id == id));
        if let Some(path) = pending_open {
            if let Err(error) = self.begin_import(path, ImportKind::Open) {
                self.settle_import_failure(error, "ebook.importer_unavailable");
            }
        } else if let Some(handed) = self.pending_handed.take() {
            let entry = content::handed_book_entry(&handed);
            self.perform_action(doc, &crate::actions::restore_library_book_action(&entry));
            let id = handed.book.id.clone();
            if let Some(hash) = &handed.content {
                self.library_paths
                    .insert(id.clone(), content::content_path(hash));
            }
            let index = if let Some(index) = self.library.iter().position(|b| b.id == id) {
                index
            } else {
                Arc::make_mut(&mut self.library).push(Arc::new(handed.book));
                self.library.len() - 1
            };
            self.open_book_at(doc, index);
        } else if let Some(index) = pending_book {
            self.open_book_at(doc, index);
        } else {
            self.view = View::Library;
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
        let Some(id) = self.library.get(self.book_idx).map(|b| b.id.clone()) else {
            return;
        };
        let mut p = BooksDoc::read_progress(doc, &id);
        let nchap = self.num_chapters().saturating_sub(1);
        self.chapter = p.chapter.min(nchap);
        self.page = p.page;
        p.opened = unix_now_ms();
        self.perform_action(doc, &crate::actions::set_progress_action(&id, p));
        self.clear_selection();
        self.composing = None;
        self.note_composer.close();
        self.comment_overlay.close();
        self.layout = None;
        self.last_persisted = None;
        self.toc_scroll_to_active = true;
        self.notes_scroll_to_annotation = None;
    }

    /// Ask the host to choose a book to open; the chosen path comes back over
    /// a channel drained each frame in [`Self::drain_portal`]. A second
    /// request while one is in flight is ignored.
    fn request_open_via_portal(&mut self) {
        if self.portal_rx.is_some() || self.portal_contents_rx.is_some() {
            return;
        }
        if crate::web_file::native_paths() {
            self.portal_rx = Some(book_open_request().send());
        } else {
            self.portal_contents_rx = Some(book_open_request().send_contents());
        }
        self.load_error = None;
        self.dirty = true;
    }

    /// Drain a completed picker request, if any. Never blocks — a
    /// still-pending picker stays installed for the next frame.
    fn drain_portal(&mut self) {
        if let Some(rx) = self.portal_contents_rx.as_ref() {
            match rx.try_recv() {
                Ok(ContentsOutcome::Picked(files)) => {
                    self.portal_contents_rx = None;
                    if let Some(file) = files.into_iter().next() {
                        crate::web_file::remember(&file.name, &file.bytes);
                        self.pending_contents = Some((file.name, file.bytes));
                    }
                }
                Ok(ContentsOutcome::Cancelled)
                | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.portal_contents_rx = None;
                }
                Ok(ContentsOutcome::Failed(reason)) => {
                    self.portal_contents_rx = None;
                    self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                    self.dirty = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => {
                        if let Err(error) = self.begin_import(path, ImportKind::Open) {
                            self.settle_import_failure(error, "ebook.importer_unavailable");
                        }
                    }
                    // A cancel is not an error — leave the current view intact.
                    PortalOutcome::Cancelled => {
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.completed_resource_invocation =
                                Some(tilo_actions::InvocationUpdate::Cancelled {
                                    invocation,
                                    message: Some("book selection cancelled".into()),
                                });
                        }
                    }
                    PortalOutcome::Failed(reason) => {
                        self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                        self.dirty = true;
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.completed_resource_invocation =
                                Some(tilo_actions::InvocationUpdate::Failed {
                                    invocation,
                                    error: tilo_actions::ActionFailure {
                                        code: "ebook.portal_failed".into(),
                                        message: reason,
                                        details: None,
                                        retryable: false,
                                    },
                                });
                        }
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                // A lifecycle operation must settle exactly once. Without
                // this branch a failed host picker could leave an agent or
                // palette invocation pending forever.
                if let Some(invocation) = self.resource_invocation.take() {
                    self.completed_resource_invocation =
                        Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "ebook.portal_disconnected".into(),
                                message: "The book picker stopped before returning a selection."
                                    .into(),
                                details: None,
                                retryable: true,
                            },
                        });
                }
                self.dirty = true;
            }
        }
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
            #[cfg(test)]
            hold: None,
        })?;
        self.import_generation = generation;
        self.import_loading = Some(LoadingImport {
            generation,
            path,
            kind,
            expected_book_id,
        });
        self.load_error = None;
        self.dirty = true;
        Ok(())
    }

    fn settle_import_failure(&mut self, message: String, code: &str) {
        self.import_loading = None;
        self.load_error = Some(message.clone());
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
    fn adopt_contents(&mut self, doc: &mut AutoCommit) {
        let Some((name, bytes)) = self.pending_contents.take() else {
            return;
        };
        match crate::epub::parse_epub(&bytes, 0, "") {
            Ok(book) => {
                let path = PathBuf::from(format!("browser/{name}"));
                let (book, entry) = content::prepare_imported_book(&path, book);
                self.perform_action(doc, &crate::actions::restore_library_book_action(&entry));
                let id = book.id.clone();
                let index = if let Some(index) = self.library.iter().position(|b| b.id == id) {
                    if let Some(slot) = Arc::make_mut(&mut self.library).get_mut(index) {
                        *slot = Arc::new(book);
                    }
                    index
                } else {
                    Arc::make_mut(&mut self.library).push(Arc::new(book));
                    self.library.len() - 1
                };
                self.load_error = None;
                self.open_book_at(doc, index);
            }
            Err(error) => self.load_error = Some(format!("Couldn’t open {name}: {error}")),
        }
        self.dirty = true;
    }

    fn drain_import_worker(&mut self, doc: &mut AutoCommit) {
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
                    self.perform_action(doc, &crate::actions::restore_library_book_action(&entry));
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
                    self.load_error = None;
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
                    self.load_error = Some(format!("Couldn’t open {name}: {error}"));
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.completed_resource_invocation =
                            Some(tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "ebook.open_failed".into(),
                                    message: self.load_error.clone().unwrap_or_default(),
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
            hold: Some((entered_tx, release_rx)),
        })?;
        self.import_generation = generation;
        self.import_loading = Some(LoadingImport {
            generation,
            path: path.to_path_buf(),
            kind,
            expected_book_id: None,
        });
        Ok((entered, release))
    }

    #[cfg(test)]
    fn import_and_open_for_test(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let book = content::parse_path_detailed(path).expect("test EPUB parses");
        let (book, entry) = content::prepare_imported_book(path, book);
        self.perform_action(doc, &crate::actions::restore_library_book_action(&entry));
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

    fn dispatch(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::OpenBook(i) => {
                self.open_book_at(doc, i);
            }
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
            Action::CloseComments => self.on_close_comments(),
            Action::DeleteHighlight(group) => self.on_delete_highlight(group, doc),
            Action::AddComment(group) => self.on_add_comment(group, doc),
            Action::EditComment(id) => self.on_edit_comment(id),
            Action::DeleteComment(id) => self.on_delete_comment(id, doc),
            Action::OpenViaPortal => self.request_open_via_portal(),
            Action::ShareToWorkspace(id) => self.on_share_to_workspace(&id, doc),
            Action::RemoveFromLibrary(id) => self.on_remove_from_library(&id, doc),
            Action::ToggleWatchFolder(folder) => self.on_toggle_watch_folder(&folder, doc),

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

    fn dispatch_reading_settings(&mut self, action: ReadingSettingsAction, doc: &mut AutoCommit) {
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
        self.toc_scroll_to_active = true;
    }

    fn on_toggle_toc(&mut self) {
        let layout = self.toc_layout();
        self.toc_flap.toggle(layout);
        if self.toc_flap.is_open(layout) {
            self.toc_scroll_to_active = true;
        }
    }

    fn on_toggle_notes(&mut self) {
        let layout = self.notes_layout();
        self.notes_flap.toggle(layout);
    }

    fn on_dismiss_overlay(&mut self) {
        self.reading_settings.close();
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
            if let Some(next) = shown.get((idx + 1) % shown.len()).map(|f| f.id.clone()) {
                self.bump_setting(doc, |s| s.font_id = next);
            }
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
        if let Some(draft) = self.selection_note_draft(String::new()) {
            let id = draft.id;
            self.perform_action(doc, &draft.into_action());
            self.composing = Some(id);
            self.note_composer.open(id, self.actions.framework.as_ref());
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
            self.note_composer.open(id, self.actions.framework.as_ref());
            self.notes_flap.open();
            self.notes_scroll_to_annotation = Some(id);
        }
    }

    fn on_jump_to(&mut self, id: Uuid) {
        if let Some(a) = self.annotations.iter().find(|a| a.id == id).cloned() {
            self.chapter = a.chapter.min(self.num_chapters().saturating_sub(1));
            self.pending_jump_block = Some(a.block);
            self.clear_selection();
            self.comment_overlay.close();
            self.layout = None;
            self.notes_scroll_to_annotation = Some(id);
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

    fn on_close_comments(&mut self) {
        self.comment_overlay.finish_editing();
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
        self.comment_overlay.finish_editing();
        let anchor = self
            .annotations
            .iter()
            .filter(|a| a.kind == AnnoKind::Highlight && a.group == group)
            .min_by_key(|a| (a.block, a.start))
            .map(|a| (a.block, a.start, a.end, a.color, a.excerpt.clone()));
        if let Some((block, start, end, color, excerpt)) = anchor {
            let book_id = self.book_id();
            let id = Uuid::new_v4();
            let action = crate::actions::CommentDraft {
                group,
                note: crate::actions::NoteDraft {
                    book_id,
                    id,
                    anchor: AnnotationAnchor {
                        chapter: self.chapter,
                        block,
                        start,
                        end,
                        excerpt,
                    },
                    color,
                    text: String::new(),
                },
            }
            .into_action();
            self.perform_action(doc, &action);
            self.comment_overlay.open(group);
            self.comment_overlay
                .begin_editing(id, self.actions.framework.as_ref());
        }
    }

    fn on_edit_comment(&mut self, id: Uuid) {
        if self
            .annotations
            .iter()
            .any(|annotation| annotation.id == id && annotation.kind == AnnoKind::Note)
        {
            self.comment_overlay
                .begin_editing(id, self.actions.framework.as_ref());
        }
    }

    fn on_delete_comment(&mut self, id: Uuid, doc: &mut AutoCommit) {
        if self.comment_overlay.editing_id() == Some(id) {
            self.comment_overlay.finish_editing();
        }
        self.perform_action(doc, &crate::actions::delete_annotation_action(id));
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
    fn begin_hydrate_shared_book(&mut self, id: &str, doc: &AutoCommit) -> Result<(), String> {
        if self.pending_share.is_some()
            || self.resource_invocation.is_some()
            || self.import_loading.is_some()
        {
            return Err("another book lifecycle operation is in progress".into());
        }
        let advertised = BooksDoc::read_library_entries(doc)
            .iter()
            .any(|entry| entry.id == id && entry.shared);
        if !advertised {
            return Err("this shared book is no longer available in the workspace".into());
        }
        self.begin_import_expected(
            content::workspace_book_path(id),
            ImportKind::Open,
            Some(id.to_string()),
        )
    }

    fn commit_pending_share(&mut self, doc: &mut AutoCommit) {
        let Some(pending) = self.pending_share.take() else {
            return;
        };
        let (book, entry) = content::prepare_imported_book(&pending.path, pending.book);
        self.perform_action(doc, &crate::actions::restore_library_book_action(&entry));
        if book.id == pending.book_id {
            self.local_library
                .remember_book(&pending.book_id, &pending.path);
            self.library_paths.insert(pending.book_id, pending.path);
            self.local_library.save();
            self.load_error = None;
        } else {
            self.load_error = Some("The shared copy no longer matches this book.".into());
        }
        if let Some(invocation) = self.resource_invocation.take() {
            self.completed_resource_invocation = Some(if self.load_error.is_none() {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "ebook.share_failed".into(),
                        message: self.load_error.clone().unwrap_or_default(),
                        details: None,
                        retryable: true,
                    },
                }
            });
        }
        self.dirty = true;
    }

    fn on_share_to_workspace(&mut self, id: &str, doc: &mut AutoCommit) {
        if let Err(error) = self.begin_share_book(id) {
            self.load_error = Some(error);
            return;
        }
        self.commit_pending_share(doc);
    }

    fn on_remove_from_library(&mut self, id: &str, doc: &mut AutoCommit) {
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
        self.perform_action(
            doc,
            &crate::actions::remove_library_book_action(&removed.id),
        );
        // Keep the reader pointed at a valid book after the shelf shrinks.
        self.book_idx = self.book_idx.min(self.library.len().saturating_sub(1));
        self.shelf_cursor = self.shelf_cursor.min(self.library.len().saturating_sub(1));
        if self.library.is_empty() {
            self.view = View::Library;
        }
        self.layout = None;
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
        self.default_font_menu = false;
        self.editing_scheme = None;
        self.renaming = None;
        self.shortcut_capture = None;
        self.shortcut_capture_error = None;
        self.settings_scroll = 0.0;
    }

    fn on_close_settings(&mut self, doc: &mut AutoCommit) {
        self.commit_rename(doc);
        self.font_picker = false;
        self.color_picker.cancel();
        self.default_font_menu = false;
        self.editing_scheme = None;
        self.shortcut_capture = None;
        self.shortcut_capture_error = None;
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
        self.shortcut_capture = None;
        self.shortcut_capture_error = None;
        self.settings_tab = tab;
        self.settings_scroll = 0.0;
    }

    fn on_toggle_scheme_enabled(&mut self, id: &str, doc: &mut AutoCommit) {
        let mut sc = self.schemes.clone();
        let mut now_disabled = false;
        let changed = if let Some(s) = sc.iter_mut().find(|s| s.id == id) {
            s.enabled = !s.enabled;
            now_disabled = !s.enabled;
            Some(s.clone())
        } else {
            None
        };
        if let Some(scheme) = changed {
            self.perform_action(doc, &crate::actions::upsert_color_scheme_action(scheme));
        }
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
            sc.insert(pos, dup.clone());
            self.perform_action(doc, &crate::actions::upsert_color_scheme_action(dup));
            self.editing_scheme = Some(new_id);
        }
    }

    fn on_delete_scheme(&mut self, id: &str, doc: &mut AutoCommit) {
        if self.schemes.iter().any(|s| s.id == id && !s.builtin) {
            self.perform_action(doc, &crate::actions::delete_color_scheme_action(id));
        }
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
            .and_then(|s| s.slots().get(slot).map(|&(_, c)| c))
            .unwrap_or([0x80, 0x80, 0x80]);
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
        self.perform_action(doc, &crate::actions::set_default_font_action(id));
        self.default_font_menu = false;
    }

    fn on_toggle_font_shown(&mut self, id: &str, doc: &mut AutoCommit) {
        if let Some(mut font) = self.fonts.iter().find(|entry| entry.id == id).cloned() {
            font.shown = !font.shown;
            self.perform_action(doc, &crate::actions::upsert_font_action(font));
        }
    }

    fn on_remove_font(&mut self, id: &str, doc: &mut AutoCommit) {
        self.perform_action(doc, &crate::actions::delete_font_action(id));
        if self.settings.font_id == id {
            self.bump_setting(doc, |s| s.font_id = String::new());
        }
        if self.default_font_id == id {
            self.perform_action(doc, &crate::actions::set_default_font_action(""));
        }
    }

    fn on_open_font_picker(&mut self) {
        self.font_picker = true;
        self.picker_scroll = 0.0;
    }

    fn on_add_system_font(&mut self, family: &str, doc: &mut AutoCommit) {
        let f = self.fonts.clone();
        let dup = f
            .iter()
            .any(|e| matches!(&e.kind, FontKind::Named(n) if n == family));
        if !dup {
            let ids: Vec<String> = f.iter().map(|e| e.id.clone()).collect();
            let id = Self::fresh_id("font", &ids);
            let font = FontEntry::new(&id, FontKind::Named(family.to_string()), family, 1.0, true);
            self.perform_action(doc, &crate::actions::upsert_font_action(font));
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
        let before = self.library_prefs;
        let mut p = before;
        p.sort = sort;
        self.library_prefs = p;
        self.perform_action(
            doc,
            &crate::actions::set_library_preferences_action(before, p),
        );
        self.shelf_controls.close_sort_menu();
    }

    fn on_toggle_hide_read(&mut self, doc: &mut AutoCommit) {
        let before = self.library_prefs;
        let mut p = before;
        p.hide_read = !p.hide_read;
        self.library_prefs = p;
        self.perform_action(
            doc,
            &crate::actions::set_library_preferences_action(before, p),
        );
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

    /// Apply a confirmed pick: create the new scheme (for a from-background
    /// pick) or write the slot edit. Nothing is written while the picker is
    /// open, so *Cancel* leaves the scheme exactly as it was.
    fn commit_picker(&mut self, doc: &mut AutoCommit, edit: ColorEdit) {
        match edit.target {
            PickTarget::NewScheme => {
                let sc = self.schemes.clone();
                let ids: Vec<String> = sc.iter().map(|s| s.id.clone()).collect();
                let new_id = Self::fresh_id("scheme", &ids);
                let n = sc.iter().filter(|s| !s.builtin).count() + 1;
                let scheme = ColorScheme::from_bg(&new_id, &format!("Scheme {n}"), edit.rgb);
                self.perform_action(doc, &crate::actions::upsert_color_scheme_action(scheme));
                self.editing_scheme = Some(new_id.clone());
                self.bump_setting(doc, |st| st.scheme_id = new_id);
            }
            PickTarget::Slot(id, slot) => {
                let mut sc = self.schemes.clone();
                if let Some(s) = sc.iter_mut().find(|s| s.id == id && !s.builtin) {
                    s.set_slot(slot, edit.rgb);
                    let scheme = s.clone();
                    self.perform_action(doc, &crate::actions::upsert_color_scheme_action(scheme));
                }
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
    fn apply_rename(&mut self, target: RenameTarget, name: &str, doc: &mut AutoCommit) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match target {
            RenameTarget::Font(id) => {
                let mut f = self.fonts.clone();
                if let Some(e) = f.iter_mut().find(|e| e.id == id) {
                    e.name = name.to_string();
                    let font = e.clone();
                    self.perform_action(doc, &crate::actions::upsert_font_action(font));
                }
            }
            RenameTarget::Scheme(id) => {
                let mut sc = self.schemes.clone();
                if let Some(s) = sc.iter_mut().find(|s| s.id == id) {
                    s.name = name.to_string();
                    let scheme = s.clone();
                    self.perform_action(doc, &crate::actions::upsert_color_scheme_action(scheme));
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

    fn bump_setting(&mut self, doc: &mut AutoCommit, f: impl FnOnce(&mut Settings)) {
        let before = BooksDoc::read_settings(doc);
        let mut s = before.clone();
        f(&mut s);
        self.perform_action(
            doc,
            &crate::actions::set_reading_settings_action(before, s.clone()),
        );
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
        // Prefer painted layout metrics; fall back to chapter block lengths so a
        // select-passage View can publish highlight-selection before the next
        // frame lays out the reader.
        let blocks: Vec<(usize, usize, String)> = if let Some(layout) = self.layout.as_ref() {
            layout
                .blocks
                .iter()
                .map(|b| (b.index, b.char_len, b.text.clone()))
                .collect()
        } else {
            let chapter = self.open_book()?.chapters.get(self.chapter)?;
            chapter
                .blocks
                .iter()
                .enumerate()
                .map(|(index, block)| {
                    let text = block.plain_text();
                    (index, text.chars().count(), text)
                })
                .collect()
        };
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
        let remove_groups: Vec<String> = overlapping.iter().map(|(g, _)| g.to_string()).collect();

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
                pieces.push(crate::actions::HiPiece {
                    id: Uuid::new_v4().to_string(),
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
                AnnotationAnchor {
                    chapter: self.chapter,
                    block: tb.index,
                    start: 0,
                    end: span,
                    excerpt,
                },
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
        self.actions.dispatch_with_context(
            doc,
            crate::actions::EBOOK_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.ebook", "Ebook", action),
        );
    }

    /// Show the shelf settings and drop books the shared library no longer lists.
    /// An empty projection does not wipe a shelf that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::EbookState) {
        if state.settings != self.settings {
            self.settings = state.settings.clone();
            self.layout = None;
        }
        self.library_prefs = state.prefs;
        if state.entries.is_empty() {
            return;
        }
        let keep: std::collections::HashSet<&str> = state
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        let before = self.library.len();
        std::sync::Arc::make_mut(&mut self.library).retain(|book| keep.contains(book.id.as_str()));
        if self.library.len() == before {
            return;
        }
        self.library_paths
            .retain(|id, _| keep.contains(id.as_str()));
        self.watched_sources
            .retain(|_, id| keep.contains(id.as_str()));
        self.book_idx = self.book_idx.min(self.library.len().saturating_sub(1));
        if self.library.is_empty() {
            self.view = View::Library;
        }
        self.layout = None;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Ebook>,
    ) {
        self.note_composer.attach(&dispatcher);
        self.comment_overlay.attach(&dispatcher);
        self.actions.attach(dispatcher);
    }

    #[allow(
        dead_code,
        reason = "called from this crate's tests; dependents compile the library without cfg(test)"
    )]
    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
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

    /// The last [`EncodedAction`](tilo_actions::EncodedAction) an on-page
    /// affordance dispatched: a control and its Ctrl+K entry must build the
    /// identical action.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
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
                    out.push_str(if keep_lines { "\n\n" } else { " " });
                }
                out.extend(b.text.chars().enumerate().skip(lo).take(hi - lo).map(
                    |(index, character)| {
                        if keep_lines && b.hard_breaks.binary_search(&index).is_ok() {
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

    fn toggle_bookmark(&mut self, doc: &mut AutoCommit) {
        let block = self.page_top_block;
        self.perform_action(doc, &self.bookmark_plan(block).into_action());
    }

    /// Resolve the bookmark's absolute post-action state at authoring time.
    /// Applying the resulting action never branches on another peer's state.
    fn bookmark_plan(&self, block: usize) -> crate::actions::BookmarkPlan {
        let remove_ids = self
            .annotations
            .iter()
            .filter(|annotation| {
                annotation.kind == AnnoKind::Bookmark
                    && annotation.chapter == self.chapter
                    && annotation.block == block
            })
            .map(|annotation| annotation.id)
            .collect::<Vec<_>>();
        crate::actions::BookmarkPlan {
            book_id: self.book_id(),
            id: Uuid::new_v4(),
            chapter: self.chapter,
            block,
            excerpt: self.block_excerpt(block),
            present: remove_ids.is_empty(),
            remove_ids,
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

    fn finish_composing(&mut self, doc: &mut AutoCommit) -> bool {
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
        self.perform_action(
            doc,
            &crate::actions::finalize_annotation_action(id, created.max(unix_now_ms())),
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
            Action::OpenBook(index) => format!("book.open:{}", self.library.get(*index)?.id),
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
            Action::SetDefaultFont(id) => format!("settings.default-font:{id}"),
            Action::ToggleDefaultFontMenu => "settings.default-font.menu".into(),
            Action::ToggleFontShown(id) => format!("settings.font.shown:{id}"),
            Action::RemoveFont(id) => format!("settings.font.remove:{id}"),
            Action::OpenFontPicker => "settings.font.add".into(),
            Action::CloseFontPicker => "settings.font-picker.close".into(),
            Action::AddSystemFont(family) => {
                format!("settings.font.add:{}", ui::a11y::hash_key(family))
            }
            Action::BeginRenameFont(id) => format!("settings.font.rename:{id}"),
            Action::DismissOverlay | Action::Swallow => return None,
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
                .library
                .get(*index)
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
            Action::SeekProgress => format!(
                "Reading progress: {}%",
                (self.reading_progress * 100.0).round() as u8
            ),
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
            Action::SetFont(id) | Action::SetDefaultFont(id) => self
                .fonts
                .iter()
                .find(|font| font.id == *id)
                .map_or_else(|| id.clone(), |font| font.name.clone()),
            Action::SetReadTheme(theme) => format!("{} reading theme", theme.label()),
            Action::SetMode(mode) => format!("{} reading mode", mode.label()),
            Action::SetComposition(style) => format!("{} text composition", style.label()),
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
                        self.local_library
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
            Action::ToggleDefaultFontMenu => "Default font".into(),
            Action::ToggleFontShown(id) => format!("Show {} in reader", self.font_name(id)),
            Action::RemoveFont(id) => format!("Remove {}", self.font_name(id)),
            Action::OpenFontPicker => "Add font".into(),
            Action::CloseFontPicker => "Close font picker".into(),
            Action::AddSystemFont(family) => format!("Add {family}"),
            Action::BeginRenameFont(id) => format!("Rename {}", self.font_name(id)),
            Action::DismissOverlay | Action::Swallow => String::new(),
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
                    let text = if annotation.note.trim().is_empty() {
                        annotation.excerpt.trim()
                    } else {
                        annotation.note.trim()
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
            Action::ToggleToc => (
                Role::ToggleButton,
                base.checked(self.toc_flap.is_open(self.toc_layout())),
            ),
            Action::ToggleNotes => (
                Role::ToggleButton,
                base.checked(self.notes_flap.is_open(self.notes_layout())),
            ),
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
            Action::SetDefaultFont(id) => {
                (Role::RadioButton, base.checked(self.default_font_id == *id))
            }
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

    fn draw_load_toast(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        message: &str,
        border: Color,
    ) {
        let r = self.rect;
        // Keep the surface wholly on-screen at phone widths.  The previous
        // `clamp(220, 460)` could choose the full viewport width, putting its
        // left edge beyond the window and clipping a two-line failure.
        let width = (r.width() - 36.0).clamp(160.0, 460.0);
        let x1 = r.x1 - 18.0;
        let text: String = message.chars().take(180).collect();
        let (_, text_h) = tr.measure(&text, 12.5, Some((width - 28.0) as f32));
        let height = (f64::from(text_h) + 24.0).clamp(48.0, 92.0);
        let toast = Rect::new(
            x1 - width,
            r.y0 + TOP_BAR + 14.0,
            x1,
            r.y0 + TOP_BAR + 14.0 + height,
        );
        // A transient Panel with a severity stripe on its leading edge: the
        // stripe carries the info/danger tone (HIG §7 "state reads at a
        // glance"), so the panel's own edge can stay the derived one.
        chrome::panel(scene, t, theme, toast);
        let stripe = Rect::new(
            toast.x0 + rhythm::RELATED,
            toast.y0 + rhythm::SIBLING,
            toast.x0 + rhythm::RELATED + 3.0,
            toast.y1 - rhythm::SIBLING,
        );
        paint::fill_round(scene, t, stripe, 1.5, border);
        tr.draw_text(
            scene,
            &text,
            12.5,
            theme.tokens.colors.text_primary,
            t * Affine::translate((toast.x0 + 14.0 + rhythm::RELATED, toast.y0 + 16.0)),
            Some((toast.width() - 28.0 - rhythm::RELATED) as f32),
        );
    }
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
            "page": self.page,
        }))
        .ok()?;
        Some(ui::collaboration::ContinuationMetadata {
            schema_version: 1,
            bytes,
        })
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        _doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if metadata.schema_version != 1 {
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
        let page = value
            .get("page")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0);
        self.book_idx = index;
        self.chapter = chapter;
        self.page = page;
        true
    }
}

impl Widget for BooksApp {
    ui::provides!(
        dyn ui::widget::Continuable,
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::EBOOK_OPERATION_TYPE]
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
            self.portal_rx = None;
            self.pending_share = None;
            let message = match self.resource_lifecycle.take() {
                Some(ResourceLifecycle::Share) => "book sharing cancelled",
                Some(ResourceLifecycle::Hydrate) => "shared book download cancelled",
                Some(ResourceLifecycle::Open) | None => "book open cancelled",
            };
            // Parsing cannot be safely interrupted mid-ZIP member, but its
            // eventual result is no longer authoritative and will be rejected
            // by the missing loading generation.
            self.import_loading = None;
            self.dirty = true;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(message.into()),
            });
        }
        if self
            .completed_resource_invocation
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_lifecycle = None;
            return self.completed_resource_invocation.take();
        }
        None
    }

    #[allow(
        clippy::too_many_lines,
        reason = "sync owns the ordered UI-thread handoff for document, portal, and lifecycle state"
    )]
    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        let (doc, path) = ui::doc::document(doc, path);
        // Hydrate the persisted library on the first frame (when the document is
        // finally available), then pull in any book the host picker returned.
        self.ensure_library_loaded(doc);
        self.close_orphaned_editors();
        self.note_composer.sync(doc, path);
        self.comment_overlay.sync(doc, path);
        self.drain_portal();
        self.adopt_contents(doc);
        self.drain_import_worker(doc);
        self.commit_pending_share(doc);
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
            self.watched_folders = wf;
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
                self.note_composer
                    .open(aid, self.actions.framework.as_ref());
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
                cm.created = unix_now_ms() + i as u64;
                BooksDoc::upsert_annotation(doc, &cm);
            }
            self.comment_overlay.open(group);
        }
        if self.shot_long_annotations {
            self.shot_long_annotations = false;
            let book_id = self.book_id();
            for index in 0..30 {
                let color = HiColor::ALL
                    .get(index % HiColor::ALL.len())
                    .copied()
                    .unwrap_or(HiColor::Yellow);
                let mut annotation = Annotation::new(&book_id, AnnoKind::Note, 0, index)
                    .with_color(color)
                    .with_excerpt(format!(
                        "Saved passage {} from the lantern keeper's journal",
                        index + 1
                    ));
                annotation.note = if index == 25 {
                    "Targeted annotation — revealed here after a jump from search.".into()
                } else {
                    format!("Reading note {} — a durable, shared thought.", index + 1)
                };
                annotation.created = unix_now_ms().saturating_add(index as u64);
                if index == 25 {
                    self.notes_scroll_to_annotation = Some(annotation.id);
                }
                BooksDoc::upsert_annotation(doc, &annotation);
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
        if self.shot_clean_reader {
            // A captured accessible-type proof should demonstrate reflow, not
            // a snapshot-specific annotation band. The source document is
            // untouched; this only changes the one-frame presentation cache.
            self.annotations.clear();
            self.all_annotations = Arc::new(Vec::new());
        }
        self.persist_progress(doc);
        let mut live_actions = vec![Self::publish_resource(self, doc, "ebook.settings")];
        if let Some(book) = self.open_book() {
            live_actions.push(Self::publish_resource(
                self,
                doc,
                &format!("ebook.book:{}", book.id),
            ));
        }
        live_actions.extend(
            self.fonts
                .iter()
                .map(|font| Self::publish_resource(self, doc, &format!("ebook.font:{}", font.id))),
        );
        live_actions.extend(self.schemes.iter().map(|scheme| {
            Self::publish_resource(self, doc, &format!("ebook.scheme:{}", scheme.id))
        }));
        let live_actions = live_actions.into_iter().flatten().collect::<Vec<_>>();
        let mut action_hasher = std::collections::hash_map::DefaultHasher::new();
        self.book_id().hash(&mut action_hasher);
        self.chapter.hash(&mut action_hasher);
        self.page_top_block.hash(&mut action_hasher);
        self.sel
            .map(|selection| {
                (
                    selection.anchor.block,
                    selection.anchor.ch,
                    selection.head.block,
                    selection.head.ch,
                )
            })
            .hash(&mut action_hasher);
        for resource in &live_actions {
            resource.id.hash(&mut action_hasher);
            for action in &resource.actions {
                action.id.hash(&mut action_hasher);
                action.label.hash(&mut action_hasher);
                action.description.hash(&mut action_hasher);
                action.operation_type.hash(&mut action_hasher);
                match &action.input {
                    tilo_actions::ActionInput::Immediate(encoded) => {
                        0_u8.hash(&mut action_hasher);
                        encoded.type_id.hash(&mut action_hasher);
                        encoded.payload.hash(&mut action_hasher);
                    }
                    tilo_actions::ActionInput::Deferred { type_id, .. } => {
                        2_u8.hash(&mut action_hasher);
                        type_id.hash(&mut action_hasher);
                    }
                    tilo_actions::ActionInput::Prompted {
                        prompt,
                        placeholder,
                        collects,
                        ..
                    } => {
                        1_u8.hash(&mut action_hasher);
                        prompt.hash(&mut action_hasher);
                        placeholder.hash(&mut action_hasher);
                        (*collects as u8).hash(&mut action_hasher);
                    }
                }
            }
        }
        let action_fingerprint = action_hasher.finish();
        if action_fingerprint != self.action_search_fingerprint {
            self.action_search_fingerprint = action_fingerprint;
            self.action_search_epoch.fetch_add(1, Ordering::AcqRel);
        }
        self.action_search_resources = Arc::new(live_actions);
        let registry = BooksDoc::read_library_entries(doc);
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
                self.search_provider = build.provider;
                self.search_fingerprint = fingerprint;
                self.search_revision = build.generation;
                self.action_search_epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
        if fingerprint != self.search_fingerprint && self.search_worker.is_none() {
            let Some(generation) = self.search_revision.checked_add(1) else {
                // Never wrap a generation: a stale palette cursor must not
                // become valid again after integer exhaustion.
                self.search_provider = crate::search_provider::BookSearchProvider::new_shared(
                    Arc::clone(&self.library),
                    Arc::clone(&self.all_annotations),
                    Arc::new(registry),
                    0,
                );
                self.action_search_epoch.fetch_add(1, Ordering::AcqRel);
                return;
            };
            let registry = Arc::new(registry);
            if let Ok(worker) = SearchWorker::spawn(
                self.search_provider.clone(),
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
                self.search_provider = crate::search_provider::BookSearchProvider::new_shared(
                    Arc::clone(&self.library),
                    Arc::clone(&self.all_annotations),
                    Arc::new(Vec::new()),
                    0,
                );
                self.action_search_epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
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
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Rail reconciliation happens at the start of `draw`, once both the
        // allocated width and the host accessibility text scale are known.
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
        self.reading_settings.begin_frame();
        // Advance the rail flap transitions (reconcile first so a resize this
        // frame uses the right mode default) before laying anything out.
        self.reconcile_flaps();
        let dt = 1.0 / 60.0;
        if theme.reduce_motion {
            let toc = self.toc_layout();
            let notes = self.notes_layout();
            self.toc_flap.snap(self.toc_flap.is_open(toc));
            self.notes_flap.snap(self.notes_flap.is_open(notes));
            self.page_turn = None;
        } else {
            self.toc_flap.tick(self.toc_layout(), dt);
            self.notes_flap.tick(self.notes_layout(), dt);
        }
        // App-base fill so nothing shows through.
        paint::fill_rect(
            scene,
            transform,
            self.rect,
            theme.tokens.colors.surfaces.base,
        );
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        match self.view {
            View::Library => self.draw_library(&mut ctx),
            View::Reader => self.draw_reader(&mut ctx),
            View::Settings => self.draw_settings(&mut ctx),
        }
        // The empty shelf has a dedicated error position beneath its primary
        // action. Every other view needs an honest loading/result surface:
        // parsing happens off-thread, so the previous book stays interactive.
        if self.view != View::Library || !self.library.is_empty() {
            if let Some(loading) = &self.import_loading {
                let name = loading
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("book");
                self.draw_load_toast(
                    scene,
                    transform,
                    tr,
                    theme,
                    &format!("Opening {name}…"),
                    theme.tokens.colors.semantic.info,
                );
            } else if let Some(error) = self.load_error.as_deref() {
                self.draw_load_toast(
                    scene,
                    transform,
                    tr,
                    theme,
                    error,
                    theme.tokens.colors.semantic.danger,
                );
            }
        }
        self.refresh_hover_tooltip();
        if let Some(focused) = self.semantic_focus.as_deref()
            && let Some(hit) = self
                .hits
                .iter()
                .rev()
                .find(|hit| self.semantic_target(&hit.action).as_deref() == Some(focused))
        {
            paint::stroke_round(
                scene,
                transform,
                hit.rect.inset(-2.0),
                f64::from(theme.tokens.radii.md),
                theme.tokens.colors.border_focus,
                2.0,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let mut input_ctx = InputEventCtx {
            event,
            scale_factor: ctx.scale_factor,
            modifiers: *ctx.modifiers,
            state: ctx.state,
            root: ctx.path,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
            handled: ctx.handled,
        };
        // The colour sheet is modal: while open every event is its own.
        if let Some(response) = self.color_picker.handle_event(&mut input_ctx) {
            if let ColorSheetResponse::Confirmed(edit) = response {
                self.commit_picker(input_ctx.state, edit);
            }
            self.refresh_from_doc(input_ctx.state);
            self.dirty = true;
            return;
        }
        if self.forward_rename_event(&mut input_ctx) {
            return;
        }
        if self.forward_shared_input_event(&mut input_ctx) {
            return;
        }

        match event {
            // Regaining focus is a natural moment to notice books that appeared
            // in a watched folder while the reader was elsewhere — no manual
            // "reload". A no-op when nothing is watched or nothing is new.
            WindowEvent::Focused(true) => self.rescan_watched(ctx.state),
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.handle_cursor_moved(p, ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press(*ctx.modifiers, ctx.state, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_release(ctx.state, ctx.handled);
            }
            WindowEvent::KeyboardInput { event: ke, .. } if ke.state == ElementState::Pressed => {
                if self.handle_key(&ke.logical_key, ctx.modifiers.state(), ctx.state) {
                    self.refresh_from_doc(ctx.state);
                    self.persist_progress(ctx.state);
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(*delta, ctx.state, ctx.handled);
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
        if let Some(error) = self.load_error.as_ref().filter(|error| !error.is_empty()) {
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
        doc: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (doc, path) = ui::doc::document(doc, path);
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
                        let mut fonts = self.fonts.clone();
                        if let Some(font) = fonts.iter_mut().find(|font| font.id == id) {
                            font.size_mult = value as f32;
                            let font = font.clone();
                            self.perform_action(doc, &crate::actions::upsert_font_action(font));
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
                .is_some_and(|rename| rename.field.perform_action(action, doc, path));
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

    fn has_pending_visual_work(&self) -> bool {
        self.import_loading.is_some()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.flaps_animating()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the loop pumping while a flap is mid-slide, a page turn is
        // easing in, or the floating page bubble is lingering/fading.
        !self.reduce_motion
            && (self.flaps_animating()
                || self.page_turn.is_some_and(|pt| !pt.done(ui::time::now()))
                || self.reader_bubble.needs_frame(ui::time::now()))
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.reduce_motion = reduced;
        if reduced {
            self.page_turn = None;
            let toc = self.toc_layout();
            let notes = self.notes_layout();
            self.toc_flap.snap(self.toc_flap.is_open(toc));
            self.notes_flap.snap(self.notes_flap.is_open(notes));
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

impl ui::search::capability::SearchSource for BooksApp {
    /// Make books, chapters and annotations findable through canonical
    /// resource view actions in the unified Ctrl+K palette.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![
            Box::new(self.search_provider.clone()),
            Box::new(
                self.search_provider
                    .action_search_provider()
                    .with_live_resources(
                        (*self.action_search_resources).clone(),
                        Arc::clone(&self.action_search_epoch),
                        self.action_search_epoch.load(Ordering::Acquire),
                    ),
            ),
        ]
    }
}

impl BooksApp {
    fn overlay_open_book_actions(
        resource: &mut tilo_actions::Resource,
        book_id: &str,
        ctx: &crate::actions::BookResourceCtx,
    ) {
        if resource.id.as_str() == book_id {
            *resource = crate::actions::current_book_actions(resource.clone(), ctx);
            return;
        }
        for child in &mut resource.children {
            Self::overlay_open_book_actions(child, book_id, ctx);
        }
    }
}

impl ui::resources::PublishesResources for BooksApp {
    /// Standalone / `--shots` / unit tests publish the library roots. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns the
    /// library Document door — keep only the live reading-selection extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let overlay_open_book = |root: &mut tilo_actions::Resource| {
            let Some(ctx) = self.current_book_resource_context() else {
                return;
            };
            let book_id = format!("ebook.book:{}", ctx.book_id);
            Self::overlay_open_book_actions(root, &book_id, &ctx);
            root.children
                .extend(crate::actions::live_reading_resources(&ctx));
        };

        if self.actions.is_hosted() {
            // Hosted: framework owns the library door; surface only exports
            // reading-selection extras. Open-book View overlays stay on
            // `resource()` lookup so DFS can still resolve without republishing
            // ebook.book beside ResourceRoot.
            let Some(ctx) = self.current_book_resource_context() else {
                return Vec::new();
            };
            return crate::actions::live_reading_resources(&ctx);
        }
        let state =
            <crate::resource::Ebook as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Ebook>(
            crate::resource::LIBRARY_RESOURCE_ID,
            "Books library",
            &state,
        );
        overlay_open_book(&mut root);
        vec![root]
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        let offset = match cursor {
            [] => 0,
            [offset] => usize::from(*offset),
            _ => return None,
        };
        if self.actions.is_hosted() {
            let resources = self.resources(doc);
            let total = resources.len();
            if offset > total {
                return None;
            }
            let take = max_results.min(total.saturating_sub(offset));
            let next_offset = offset.saturating_add(take);
            return Some(ui::widget::ActionDirectoryResourcePage {
                resources: resources.into_iter().skip(offset).take(take).collect(),
                next_cursor: if next_offset < total {
                    vec![next_offset as u8]
                } else {
                    Vec::new()
                },
                exhausted: next_offset >= total,
            });
        }
        let resources = self.resources(doc);
        let total = resources.len();
        if offset > total {
            return None;
        }
        let take = max_results.min(total.saturating_sub(offset));
        let next_offset = offset.saturating_add(take);
        Some(ui::widget::ActionDirectoryResourcePage {
            resources: resources.into_iter().skip(offset).take(take).collect(),
            next_cursor: if next_offset < total {
                vec![next_offset as u8]
            } else {
                Vec::new()
            },
            exhausted: next_offset >= total,
        })
    }
    fn action_directory_revision(&self, _doc: &AutoCommit) -> Option<String> {
        Some(format!("ebook-search:{}", self.search_revision))
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        // The framework root already owns the library door.
        if self.is_hosted() && id == "ebook.library" {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::CommittedActions for BooksApp {
    /// Hand the host the heads captured before an on-page affordance
    /// dispatched its annotation action. Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl BooksApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        use crate::actions::EbookOperation;

        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        let expected = crate::actions::operation_kind(&operation);
        if kind != expected {
            return Err(format!(
                "operation kind {kind:?} does not match ebook operation kind {expected:?}"
            ));
        }
        match operation {
            EbookOperation::PreviousPage => self.on_prev_page(),
            EbookOperation::NextPage => self.on_next_page(),
            EbookOperation::ToggleContents => self.on_toggle_toc(),
            EbookOperation::ToggleNotes => self.on_toggle_notes(),
            EbookOperation::OpenSettings => self.on_open_settings(),
            EbookOperation::Reveal {
                book_id,
                chapter,
                page,
                block,
            } => {
                let Some(index) = self.library.iter().position(|book| book.id == book_id) else {
                    return Err("book is no longer in the library".into());
                };
                let Some(book) = self.library.get(index) else {
                    return Err("book is no longer in the library".into());
                };
                // A resource address is an authority-bearing snapshot. Do not
                // silently clamp a chapter or annotation target from an old
                // search/resource row: a peer may have replaced the EPUB, and
                // opening a nearby passage would be materially misleading.
                if book.chapters.is_empty() {
                    if chapter != 0 || block.is_some() {
                        return Err("book no longer contains the requested reading target".into());
                    }
                } else {
                    let Some(chapter_item) = book.chapters.get(chapter) else {
                        return Err("chapter is no longer available in this book".into());
                    };
                    if let Some(block) = block
                        && block >= chapter_item.blocks.len()
                    {
                        return Err(
                            "annotation target is no longer available in this chapter".into()
                        );
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
                // Explicit, like `select_passage`: persisted, never re-adopted.
                self.layout = None;
                self.dirty = true;
            }
            EbookOperation::SelectPassage {
                book_id,
                chapter,
                start_block,
                start_char,
                end_block,
                end_char,
            } => self.select_passage(
                &book_id,
                chapter,
                (start_block, start_char),
                (end_block, end_char),
            )?,
            EbookOperation::OpenBook => {
                return Err("book open requires the lifecycle-capable operation host".into());
            }
            EbookOperation::ShareBook { .. } | EbookOperation::HydrateSharedBook { .. } => {
                return Err("book sharing requires the lifecycle-capable operation host".into());
            }
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
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        let expected = crate::actions::operation_kind(&operation);
        if kind != expected {
            return Err(format!(
                "operation kind {kind:?} does not match ebook operation kind {expected:?}"
            ));
        }
        match operation {
            crate::actions::EbookOperation::OpenBook => {
                if self.resource_invocation.is_some() {
                    return Err("another book open is in progress".into());
                }
                self.resource_invocation = Some(invocation);
                self.resource_lifecycle = Some(ResourceLifecycle::Open);
                self.request_open_via_portal();
                if self.portal_rx.is_none() {
                    self.resource_invocation = None;
                    self.resource_lifecycle = None;
                    return Err("book picker could not be started".into());
                }
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::EbookOperation::ShareBook { book_id } => {
                self.begin_share_book(&book_id)?;
                self.resource_invocation = Some(invocation);
                self.resource_lifecycle = Some(ResourceLifecycle::Share);
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::EbookOperation::HydrateSharedBook { book_id } => {
                self.begin_hydrate_shared_book(&book_id, doc)?;
                self.resource_invocation = Some(invocation);
                self.resource_lifecycle = Some(ResourceLifecycle::Hydrate);
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
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
            self.finish_composing(ctx.state);
            input_handled = true;
        }

        let settings_actions = self
            .reading_settings
            .handle_event(&mut ctx.reborrow(&mut input_handled));
        for action in settings_actions {
            self.dispatch_reading_settings(action, ctx.state);
        }

        let changes = self
            .number_controls
            .handle_event(&mut ctx.reborrow(&mut input_handled));
        for (field, value) in changes {
            match field {
                NumberField::FontScale => {
                    self.bump_setting(ctx.state, |settings| {
                        settings.font_scale = (value / 100.0) as f32;
                    });
                }
                NumberField::LineSpacing => {
                    self.bump_setting(ctx.state, |settings| settings.line_spacing = value as f32);
                }
                NumberField::FontMult(id) => {
                    let mut fonts = self.fonts.clone();
                    if let Some(font) = fonts.iter_mut().find(|font| font.id == id) {
                        font.size_mult = value as f32;
                        let font = font.clone();
                        self.perform_action(ctx.state, &crate::actions::upsert_font_action(font));
                    }
                }
            }
        }
        if input_handled {
            self.refresh_from_doc(ctx.state);
            self.dirty = true;
            *ctx.handled = true;
        }
        input_handled
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
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: &ctx.modifiers,
                    state: ctx.state,
                    path: ctx.root,
                    handled: &mut field_handled,
                    focus_target: &mut field_focus,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
            );
        }
        self.poll_rename_outcome(ctx.state);
        if field_handled {
            self.refresh_from_doc(ctx.state);
            self.dirty = true;
            *ctx.handled = true;
            return true;
        }
        false
    }

    fn handle_cursor_moved(&mut self, p: Point, doc: &mut AutoCommit, handled: &mut bool) {
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
    fn handle_left_press_overlay(&mut self, doc: &mut AutoCommit, handled: &mut bool) -> bool {
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

    fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
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

        if matches!(
            top.as_ref().map(|hit| &hit.action),
            Some(Action::SeekProgress)
        ) {
            self.progress_dragging = true;
            self.on_seek_progress(self.progress_fraction_at(self.cursor));
            self.dirty = true;
            *handled = true;
            return;
        }

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

    fn handle_left_release(&mut self, doc: &mut AutoCommit, handled: &mut bool) {
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

    fn reader_shortcut_matches(&self, shortcut: ReaderShortcut, pressed: &str) -> bool {
        self.local_library
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
                    .local_library
                    .set_shortcut(shortcut.id(), character.as_str())
                {
                    self.local_library.save();
                    self.shortcut_capture = None;
                    self.shortcut_capture_error = None;
                } else if crate::local_library::reserved_reader_key(character.as_str()) {
                    self.shortcut_capture_error = Some(format!(
                        "{} is reserved for a built-in reader command. Choose another key.",
                        character.to_uppercase()
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
    fn handle_key(&mut self, key: &Key, mods: ModifiersState, doc: &mut AutoCommit) -> bool {
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
        matches!(key, Key::Named(NamedKey::Escape)) && self.finish_composing(doc)
    }

    fn handle_progress_slider_key(&mut self, key: &Key, doc: &mut AutoCommit) -> bool {
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

    fn handle_reader_key(&mut self, key: &Key, mods: ModifiersState, doc: &mut AutoCommit) -> bool {
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
                let toc_overlay = self.toc_layout() == SplitLayout::Overlay && self.toc_open();
                let notes_overlay =
                    self.notes_layout() == SplitLayout::Overlay && self.notes_open();
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
        reason = "each iteration subtracts a fixed SWIPE_STEP_PX and breaks immediately \
                  if a turn made no progress, so the loop is bounded regardless of float \
                  imprecision"
    )]
    fn handle_swipe(&mut self, dx: f64, doc: &mut AutoCommit) -> bool {
        if dx == 0.0 || self.view != View::Reader {
            return false;
        }
        if self.reading_settings.is_open() || self.composing.is_some() {
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

// --- Small chrome-drawing primitives -----------------------------------

impl BooksApp {
    /// An icon button filling `rect` (square in practice).
    fn icon_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        ico: Ico,
        active: bool,
        action: Action,
    ) {
        let theme = ctx.theme;
        let hover = self.is_hover(rect);
        let c = &theme.tokens.colors;
        // A toggled icon button is "chosen", so it is a lift with the derived
        // hairline and glow (HIG §9.3) — never an accent wash, which would
        // read as live state on this bar.
        chrome::row(
            ctx.scene,
            ctx.transform,
            theme,
            rect,
            f64::from(theme.tokens.radii.md),
            SelectionState::new(active, hover, false),
            false,
        );
        let icon_col = if active || hover {
            c.text_primary
        } else {
            c.text_dim
        };
        let w = 1.9;
        let inner = rect;
        match ico {
            Ico::Back => paint::chevron_left(ctx.scene, ctx.transform, inner, icon_col, w),
            Ico::Toc => paint::list(ctx.scene, ctx.transform, inner, icon_col, w),
            Ico::Notes => paint::note(ctx.scene, ctx.transform, inner, icon_col, w),
            Ico::Bookmark(filled) => {
                paint::bookmark(ctx.scene, ctx.transform, inner, icon_col, w, filled);
            }
            Ico::ChevDown => paint::chevron_down(ctx.scene, ctx.transform, inner, icon_col, w),
        }
        self.push_hit(rect, action);
    }

    /// A filled accent button with a centred label (and an optional leading `+`),
    /// the single primary action on a screen. Pushes a hit for `action`.
    fn accent_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        label: &str,
        with_plus: bool,
        action: Action,
    ) {
        let theme = ctx.theme;
        // The shared primary face (HIG §8): hover, ink contrast and the
        // icon ↔ label gap all come from the toolkit instead of a white veil.
        let radius = (rect.height() / 2.0).min(f64::from(theme.tokens.radii.lg) + 4.0);
        let mut face = ButtonFace::new(rect, label)
            .primary()
            .radius(radius)
            .weight(TextWeight::Semibold)
            .text_size(14.0)
            .hovered(self.is_hover(rect));
        if with_plus {
            face = face.icon(ui::icons::Icon::Plus);
        }
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        self.push_hit(rect, action);
    }

    /// Draw a shared toggle face and retain the app-specific action routing.
    fn switch(&mut self, ctx: &mut PaintCtx<'_>, track: Rect, on: bool, action: Action) {
        let theme = ctx.theme;
        Toggle::paint(
            ctx.scene,
            ctx.transform,
            theme,
            &ToggleFace::new(track, on).hovered(self.is_hover(track)),
        );
        self.push_hit(pointer_target(track, theme), action);
    }

    /// A compact shared numeric control, laid out right-to-left from
    /// `right_mid` (its right edge, at its vertical centre); returns the left
    /// edge it consumed so every font row stays aligned.
    fn size_stepper(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        right_mid: Point,
        value: f64,
        field: &NumberField,
    ) -> f64 {
        let theme = ctx.theme;
        let (right_x, cy) = (right_mid.x, right_mid.y);
        let btn = SIZE_STEPPER_BTN;
        let by = cy - btn / 2.0;
        let width = Self::size_stepper_width(ctx.text, theme, field);
        let minus_x = right_x - width;
        self.number_controls.draw(
            field,
            value,
            Rect::new(minus_x, by, right_x, by + btn),
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
        );
        minus_x
    }

    /// Width of the stepper's value slot. The shared `NumberInput` paints its
    /// value at the toolkit field size (the theme's `lg` step at 1×, scaled
    /// once by the text scale) with `padding_standard` on either side, so the
    /// slot is measured from the widest value the field can show. The theme
    /// token is a resolved size, so it goes through `renderer_input_style`
    /// before the renderer applies its own text scale (HIG §7a). A fixed 52px
    /// slot clipped "0.96 ×" to "0.96 ›" whenever the digits ran wide.
    fn size_stepper_slot_w(tr: &mut TextRenderer, theme: &Theme, field: &NumberField) -> f64 {
        let style = tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.lg));
        let widest = field
            .widest_displays()
            .iter()
            .map(|sample| f64::from(tr.measure_style(sample, None, &style).0))
            .fold(0.0, f64::max);
        widest + 2.0 * f64::from(theme.padding_standard)
    }

    /// Total width of a size stepper: two square buttons, the measured value
    /// slot and the gaps between them. `size_stepper` lays out with this and
    /// `font_row_controls_x0` reserves space from it, so the two agree.
    fn size_stepper_width(tr: &mut TextRenderer, theme: &Theme, field: &NumberField) -> f64 {
        SIZE_STEPPER_BTN * 2.0 + Self::size_stepper_slot_w(tr, theme, field) + 8.0
    }

    /// A quiet text pill `h` tall with its top-left at `origin`, invoking
    /// `action`. Returns its width.
    fn pill(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        h: f64,
        label: &str,
        size: f32,
        action: Action,
    ) -> f64 {
        let theme = ctx.theme;
        let (x, y) = (origin.x, origin.y);
        let (tw, th) = ctx
            .text
            .measure_weighted(label, size, None, TextWeight::Medium);
        let pad = f64::from(theme.padding_standard);
        let w = f64::from(tw) + pad * 2.0;
        let rect = Rect::new(x, y, x + w, y + h);
        let hover = self.is_hover(rect);
        let c = &theme.tokens.colors;
        // Chip quiet (HIG §9.5): a raised face with its derived edge at rest.
        chrome::quiet_pill(
            ctx.scene,
            ctx.transform,
            theme,
            rect,
            h / 2.0,
            c.surfaces.raised,
            hover,
        );
        // Centre the measured line box (which carries the text scale), not the
        // nominal size — the latter sat the label low and clipped it at 2×.
        let ty = y + (h - f64::from(th)) / 2.0;
        ctx.text.draw_text_weighted(
            ctx.scene,
            label,
            size,
            c.text_primary,
            ctx.transform * Affine::translate((x + pad, ty)),
            None,
            TextWeight::Medium,
        );
        self.push_hit(rect, action);
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

impl ShelfGrid {
    /// Where grid slot `i` (in display order) sits this frame.
    fn cell(&self, i: usize) -> CardCell {
        let col = i % self.cols;
        let row = i / self.cols;
        CardCell {
            x: self.start_x + col as f64 * (self.card_w + self.gap),
            y: self.start_y + row as f64 * self.row_pitch - self.scroll,
            w: self.card_w,
            cover_h: self.cover_h,
        }
    }
}

/// One shelf card's slot: its top-left corner (scrolled), the grid's card
/// width and the cover height. Every card painter lays out from it.
#[derive(Clone, Copy)]
struct CardCell {
    x: f64,
    y: f64,
    w: f64,
    cover_h: f64,
}

impl CardCell {
    fn cover(self) -> Rect {
        Rect::new(self.x, self.y, self.x + self.w, self.y + self.cover_h)
    }
}

impl BooksApp {
    fn draw_library(&mut self, ctx: &mut PaintCtx<'_>) {
        self.shelf_controls.begin_frame();
        let lib = std::mem::take(&mut self.library);
        let r = self.rect;

        let open_x = self.draw_library_header(ctx, r, lib.len());

        // --- empty shelf: a composed, inviting onboarding -----------------
        if lib.is_empty() {
            self.draw_empty_library(ctx);
            self.library = lib;
            return;
        }

        let sort_btn =
            self.shelf_controls
                .draw_bar(ctx, r, open_x, self.cursor, self.library_prefs);

        // --- filter + sort the display order ------------------------------
        let order = self.shelf_display_order(&lib);
        self.shelf_order.clone_from(&order);

        // Everything hidden by the "hide read" filter.
        if order.is_empty() {
            Self::draw_all_read_empty_state(ctx, r);
            self.shelf_controls
                .draw_menu(ctx, r, sort_btn, self.cursor, self.library_prefs.sort);
            self.library = lib;
            return;
        }

        self.draw_library_grid(ctx, &lib, &order, r);
        self.shelf_controls
            .draw_menu(ctx, r, sort_btn, self.cursor, self.library_prefs.sort);
        self.library = lib;
    }

    /// Top bar: title + book count, and the primary "Open book" action
    /// (always present — a way in that doesn't depend on any folder).
    /// Returns the button's left edge, so shelf controls can lay out from it.
    fn draw_library_header(&mut self, ctx: &mut PaintCtx<'_>, r: Rect, book_count: usize) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        ctx.fill_rect(bar, c.surfaces.raised);
        chrome::bar_edge(
            ctx.scene,
            ctx.transform,
            c.surfaces.raised,
            r.x0,
            r.x1,
            r.y0 + TOP_BAR,
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            "Library",
            20.0,
            c.text_primary,
            ctx.transform * Affine::translate((r.x0 + 28.0, r.y0 + 9.0)),
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
            ctx.text.draw_text(
                ctx.scene,
                &sub,
                12.0,
                c.text_dim,
                ctx.transform * Affine::translate((r.x0 + 29.0, r.y0 + 32.0)),
                None,
            );
        }

        // The primary "Open book" action is always present, so there is always a
        // way in that doesn't depend on any folder — right-aligned, accent-filled.
        let compact = r.width() < 560.0;
        let btn_cy = r.y0 + (TOP_BAR - 34.0) / 2.0;
        let open_label = if compact { "Open" } else { "Open book" };
        let (olw, _) = ctx
            .text
            .measure_weighted(open_label, 14.0, None, TextWeight::Semibold);
        let open_w = f64::from(olw) + 25.0 + 32.0;
        // The bar's trailing gutter is one section step on every width.
        let open_x = r.x1 - rhythm::SECTION - open_w;
        self.accent_button(
            ctx,
            Rect::new(open_x, btn_cy, open_x + open_w, btn_cy + 34.0),
            open_label,
            true,
            Action::OpenViaPortal,
        );
        open_x
    }

    /// Everything hidden by the "hide read" filter: an empty-shelf message
    /// with a hint to turn the filter back off.
    fn draw_all_read_empty_state(ctx: &mut PaintCtx<'_>, r: Rect) {
        let c = &ctx.theme.tokens.colors;
        let cx = f64::midpoint(r.x0, r.x1);
        let cy = (r.y0 + TOP_BAR + r.y1) / 2.0;
        let head = "All books are finished";
        let (hw, _) = ctx
            .text
            .measure_weighted(head, 18.0, None, TextWeight::Semibold);
        ctx.text.draw_text_weighted(
            ctx.scene,
            head,
            18.0,
            c.text_primary,
            ctx.transform * Affine::translate((cx - f64::from(hw) / 2.0, cy - 20.0)),
            None,
            TextWeight::Semibold,
        );
        let hint = "Turn off \u{201C}Hide read\u{201D} to see them again.";
        let (tw2, _) = ctx.text.measure(hint, 13.0, None);
        ctx.text.draw_text(
            ctx.scene,
            hint,
            13.0,
            c.text_dim,
            ctx.transform * Affine::translate((cx - f64::from(tw2) / 2.0, cy + 8.0)),
            None,
        );
    }

    /// Column count, cell pitch, and vertical scroll extent/offset for the
    /// book grid. Also reveals the keyboard-focused card if a step just
    /// moved the cursor, and clamps the persisted scroll to the new extent.
    fn shelf_grid_geometry(&mut self, r: Rect, book_count: usize) -> ShelfGrid {
        // Cards are siblings before the next group: one `s6` between them,
        // and the shelf keeps a page gutter plus a section step on each side.
        let margin = rhythm::GROUP + rhythm::SECTION;
        let gap = rhythm::GROUP;
        // Keep a short shelf intentional on a wide display: a centred,
        // bounded reading-library row is easier to scan than a tiny cluster
        // marooned in the upper-left of a 1440px canvas. Cards gain only a
        // modest desktop size bump rather than stretching to fill columns.
        let desktop = r.width() >= 960.0;
        let card_w = if desktop { 212.0 } else { 196.0 };
        let cover_h = if desktop { 248.0 } else { 232.0 };
        let card_h = cover_h + 78.0;
        let max_grid_w = (r.width() - 2.0 * margin).min(1200.0).max(card_w);
        let max_cols = ((max_grid_w + gap) / (card_w + gap)).floor().max(1.0) as usize;
        // Reserve the trailing add tile while choosing columns, then centre
        // the actual used row. Large libraries still grow into the bounded
        // grid; a small shelf reads as a deliberate collection.
        let cols = max_cols.min(book_count.saturating_add(1)).max(1);
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
    #[allow(
        clippy::many_single_char_names,
        reason = "t/c/r are the standard transform/colours/rect draw-context names, and i/x/y \
                  are the grid loop's index and card position"
    )]
    fn draw_library_grid(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        lib: &[Arc<Book>],
        order: &[usize],
        r: Rect,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let grid = self.shelf_grid_geometry(r, order.len());

        // Clip the scrolling body to below the top bar so cards slide under it
        // rather than over it, then offset every card by the scroll.
        let body = Rect::new(r.x0, r.y0 + TOP_BAR + 1.0, r.x1, r.y1);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        for (i, &orig) in order.iter().enumerate() {
            let Some(book) = lib.get(orig) else { continue };
            let cell = grid.cell(i);
            // Cull rows fully outside the viewport (keeps a huge shelf cheap).
            if cell.y > r.y1 || cell.y + grid.card_h + 84.0 < r.y0 + TOP_BAR {
                continue;
            }
            let focused = i == self.shelf_cursor;
            self.draw_book_card(ctx, book, orig, cell, focused);
        }
        // Trailing "Add books" tile in the next free grid cell.
        {
            let cell = grid.cell(order.len());
            if !(cell.y > r.y1 || cell.y + grid.card_h + 84.0 < r.y0 + TOP_BAR) {
                self.draw_add_book_card(ctx, cell);
            }
        }
        ctx.scene.pop_layer();

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
            ctx.fill_round(thumb, 2.0, c.text_dim.with_alpha_factor(0.5));
        }
    }

    /// The onboarding shown when the library is empty: an emblem, a warm
    /// headline, a plain-language explanation, and one primary action — open a
    /// book from anywhere. No magic folder, no "reload"; the shelf fills itself
    /// the moment the reader opens their first book.
    fn draw_empty_library(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let r = self.rect;
        let cx = f64::midpoint(r.x0, r.x1);
        // Compose the block around a comfortable optical centre (a touch above
        // true centre reads better for a stacked call-to-action).
        let cy = r.y0 + TOP_BAR + (r.y1 - r.y0 - TOP_BAR) * 0.42;

        // Emblem: a soft accent disc with an open-book mark and a small plus.
        let disc_r = 40.0;
        let disc_cy = cy - 96.0;
        paint::circle(
            ctx.scene,
            ctx.transform,
            Point::new(cx, disc_cy),
            disc_r,
            theme.accent_primary.with_alpha_factor(0.14),
        );
        paint::book(
            ctx.scene,
            ctx.transform,
            Rect::new(cx - 20.0, disc_cy - 15.0, cx + 20.0, disc_cy + 15.0),
            theme.accent_primary,
            2.0,
        );
        paint::circle(
            ctx.scene,
            ctx.transform,
            Point::new(cx + 26.0, disc_cy + 24.0),
            11.0,
            theme.accent_primary,
        );
        paint::plus(
            ctx.scene,
            ctx.transform,
            Rect::new(cx + 18.0, disc_cy + 16.0, cx + 34.0, disc_cy + 32.0),
            theme.on_surface(theme.accent_primary).ink,
            1.9,
        );

        let head = "Your library is empty";
        let (hw, _) = ctx
            .text
            .measure_weighted(head, 24.0, None, TextWeight::Bold);
        ctx.text.draw_text_weighted(
            ctx.scene,
            head,
            24.0,
            c.text_primary,
            ctx.transform * Affine::translate((cx - f64::from(hw) / 2.0, cy - 34.0)),
            None,
            TextWeight::Bold,
        );

        let body = "Open a book from anywhere — your device or a shared workspace. It's added to your library, so it's waiting here next time.";
        let body_w = (r.width() * 0.6).clamp(280.0, 460.0);
        let (_bw, _) = ctx.text.measure(body, 14.0, Some(body_w as f32));
        ctx.text.draw_text(
            ctx.scene,
            body,
            14.0,
            c.text_dim,
            ctx.transform * Affine::translate((cx - body_w / 2.0, cy + 2.0)),
            Some(body_w as f32),
        );

        // Primary call to action.
        let cta_label = "Open a book…";
        let (clw, _) = ctx
            .text
            .measure_weighted(cta_label, 14.0, None, TextWeight::Semibold);
        let cta_w = f64::from(clw) + 25.0 + 44.0;
        let cta_h = 44.0;
        let cta_y = cy + 62.0;
        self.accent_button(
            ctx,
            Rect::new(cx - cta_w / 2.0, cta_y, cx + cta_w / 2.0, cta_y + cta_h),
            cta_label,
            true,
            Action::OpenViaPortal,
        );

        // A quiet pointer to where watched folders are managed (secondary, never
        // the only way in).
        let hint = "Tip: add folders to watch in Settings › Library.";
        let (thw, _) = ctx.text.measure(hint, 12.5, None);
        ctx.text.draw_text(
            ctx.scene,
            hint,
            12.5,
            c.text_dim.with_alpha_factor(0.75),
            ctx.transform * Affine::translate((cx - f64::from(thw) / 2.0, cta_y + cta_h + 20.0)),
            None,
        );

        // Surface a picker failure (dismissed/unavailable) right under the CTA.
        if let Some(err) = &self.load_error {
            let (ew, _) = ctx.text.measure(err, 12.5, Some((body_w) as f32));
            ctx.text.draw_text(
                ctx.scene,
                err,
                12.5,
                c.semantic.danger,
                ctx.transform
                    * Affine::translate((
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
            .filter(|&i| !self.library_prefs.hide_read || frac(i) < 0.985)
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

    fn draw_book_card(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        book: &Book,
        idx: usize,
        cell: CardCell,
        focused: bool,
    ) {
        let theme = ctx.theme;
        let CardCell { x, y, w, cover_h } = cell;
        let cover = cell.cover();
        // `self.library` is taken for the duration of the draw, so resolve
        // shared-vs-local from the book's id + the path cache directly.
        let shared = self
            .library_paths
            .get(&book.id)
            .is_some_and(|p| is_shared(p));
        let hover = self.is_hover(Rect::new(x, y, x + w, y + cover_h + 78.0));

        // Elevation from the token ramp: the shadow is the shape of the cover
        // (HIG §9.3), not a nudged black rectangle.
        ui::surface::shadow(
            ctx.scene,
            ctx.transform,
            cover,
            12.0,
            &theme.tokens.elevation.e2,
        );

        let title_on_cover = self.draw_book_cover_art(ctx, book, cell);

        // Shared-vs-local badge, overlaid at the cover's top-right. A book on the
        // workspace is available to everyone there; a local one lives only on
        // this device. Both are labelled so the state reads at a glance.
        Self::draw_shared_badge(ctx, cell, shared);

        // Keyboard focus draws the focus ring even without a pointer over the
        // card, so arrow-key navigation is visible; hover strengthens the
        // card's own derived edge to the Panel border rather than borrowing
        // the focus colour.
        if focused {
            ctx.stroke_round(cover, 12.0, theme.border_focus, 2.0);
        } else if hover {
            ctx.stroke_round(cover, 12.0, ui::chrome::panel_border(theme), 1.5);
        }

        // --- under-cover meta: progress + counts -------------------------
        let meta_y = self.draw_book_card_meta(ctx, book, cell, title_on_cover);

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
            self.draw_book_card_hover_actions(ctx, cell, &book.id, shared);
        }
    }

    /// The cover art: real embedded artwork when the EPUB carries one,
    /// otherwise a generated spine (tinted board, sheen + foot for depth,
    /// title in serif display, author along the base). Returns whether the
    /// title is already shown on the cover (a real cover isn't relabelled
    /// below it).
    fn draw_book_cover_art(&mut self, ctx: &mut PaintCtx<'_>, book: &Book, cell: CardCell) -> bool {
        let theme = ctx.theme;
        let CardCell { x, y, w, cover_h } = cell;
        let cover = cell.cover();
        let real_cover = self.cover_image(book);
        let title_on_cover = real_cover.is_none();
        if let Some(img) = real_cover {
            blit_cover(ctx.scene, ctx.transform, img, cover, 12.0);
            // Seat the art against the shelf: the edge is derived from the
            // shelf ground and stroked just outside the art, on that ground,
            // so a dark cover on a light shelf still gets a frame.
            chrome::seat(
                ctx.scene,
                ctx.transform,
                theme.tokens.colors.surfaces.base,
                cover,
                12.0,
            );
        } else {
            // A designed cover: tinted board, top sheen + darker foot for
            // depth, title in serif display, author along the base.
            let (spine, ink) = cover_colors(theme, book.cover_tint);
            ctx.fill_round(cover, 12.0, spine);
            // The board's own rest edge, derived from the board colour.
            chrome::edge(ctx.scene, ctx.transform, spine, cover, 12.0);
            // Top sheen (a faint light band) + darker foot, layered for depth.
            let sheen = Rect::new(x, y, x + w, y + cover_h * 0.34);
            ctx.fill_round(sheen, 12.0, Color::from_rgba8(255, 255, 255, 16));
            let foot = Rect::new(x, y + cover_h * 0.70, x + w, y + cover_h);
            ctx.fill_round(foot, 12.0, Color::from_rgba8(0, 0, 0, 46));

            // subject pill + spark.
            if !book.subject.is_empty() {
                let psize = 11.0;
                let (pw, _) = ctx.text.measure(&book.subject, psize, None);
                let pill = Rect::new(
                    x + 16.0,
                    y + 16.0,
                    x + 16.0 + f64::from(pw) + 18.0,
                    y + 16.0 + 20.0,
                );
                ctx.fill_round(pill, 10.0, ink.with_alpha_factor(0.18));
                ctx.text.draw_text(
                    ctx.scene,
                    &book.subject,
                    psize,
                    ink,
                    ctx.transform * Affine::translate((x + 25.0, y + 20.0)),
                    None,
                );
            }

            // Title in a serif display face (a book-cover convention), wrapped.
            let title_w = w - 32.0;
            ctx.text.draw_style(
                ctx.scene,
                &book.title,
                ink,
                ctx.transform * Affine::translate((x + 18.0, y + cover_h * 0.38)),
                Some(title_w as f32),
                &TextStyle::new(20.0)
                    .with_weight(TextWeight::Bold)
                    .with_family(FontChoice::Serif),
            );
            // Hairline rule above the author.
            paint::line(
                ctx.scene,
                ctx.transform,
                Point::new(x + 18.0, y + cover_h - 42.0),
                Point::new(x + 18.0 + 34.0, y + cover_h - 42.0),
                ink.with_alpha_factor(0.7),
                1.4,
            );
            ctx.text.draw_text(
                ctx.scene,
                &book.author,
                13.0,
                ink.with_alpha_factor(0.86),
                ctx.transform * Affine::translate((x + 18.0, y + cover_h - 30.0)),
                Some(title_w as f32),
            );
        }
        title_on_cover
    }

    /// The "Shared"/"Local" badge overlaid at the cover's top-right.
    fn draw_shared_badge(ctx: &mut PaintCtx<'_>, cell: CardCell, shared: bool) {
        let theme = ctx.theme;
        let CardCell { x, y, w, .. } = cell;
        // "Shared" is live collaborative state, so it wears the accent (HIG
        // §9.4); "Local" is a quiet overlay chip with its derived edge, and
        // both take their ink from the surface they sit on.
        let c = &theme.tokens.colors;
        let (label, bg, glyph) = if shared {
            ("Shared", theme.accent_primary, true)
        } else {
            ("Local", c.surfaces.overlay, false)
        };
        let ink = theme.on_surface(bg).ink;
        let (lw, _) = ctx
            .text
            .measure_weighted(label, 11.0, None, TextWeight::Semibold);
        let gly = if glyph { 16.0 } else { 0.0 };
        let pw = 16.0 + gly + f64::from(lw);
        let py = y + 14.0;
        let pill = Rect::new(x + w - 14.0 - pw, py, x + w - 14.0, py + 20.0);
        if shared {
            ctx.fill_round(pill, 10.0, bg);
        } else {
            ui::surface::card(ctx.scene, ctx.transform, pill, 10.0, bg);
        }
        let mut tx = pill.x0 + 8.0;
        if glyph {
            paint::people(
                ctx.scene,
                ctx.transform,
                Rect::new(tx, py + 4.0, tx + 13.0, py + 16.0),
                ink,
                1.4,
            );
            tx += 16.0;
        }
        ctx.text.draw_text_weighted(
            ctx.scene,
            label,
            11.0,
            ink,
            ctx.transform * Affine::translate((tx, py + 4.0)),
            None,
            TextWeight::Semibold,
        );
    }

    /// Under-cover meta: title (when not already on the cover), progress
    /// bar + percent, and an annotation-count badge. Returns the meta
    /// block's top y, so the caller can size the card's click target.
    #[allow(
        clippy::many_single_char_names,
        reason = "t/x/y/w/c are the standard transform/geometry/colours names shared by \
                  every draw helper in this section"
    )]
    fn draw_book_card_meta(
        &self,
        ctx: &mut PaintCtx<'_>,
        book: &Book,
        cell: CardCell,
        title_on_cover: bool,
    ) -> f64 {
        let theme = ctx.theme;
        let CardCell { x, y, w, cover_h } = cell;
        let c = &theme.tokens.colors;
        let frac = self.shelf_fraction.get(&book.id).copied().unwrap_or(0.0);
        let anno = self.shelf_annos.get(&book.id).copied().unwrap_or(0);
        let meta_y = y + cover_h + 14.0;
        // The generated cover already carries the title in display type; drawing
        // it again here would show the same title twice on one card. Only label
        // real cover art (whose embedded title may not be legible on the shelf).
        if !title_on_cover {
            ctx.text.draw_text_weighted(
                ctx.scene,
                &book.title,
                14.0,
                c.text_primary,
                ctx.transform * Affine::translate((x + 2.0, meta_y)),
                Some((w - 4.0) as f32),
                TextWeight::Semibold,
            );
        }
        // progress bar: the track is a lift off the shelf with its derived
        // edge (HIG §9.5), not a ramp slot that vanishes on one preset.
        let bar = Rect::new(x + 2.0, meta_y + 34.0, x + w - 56.0, meta_y + 40.0);
        let track = c.lift(1.0);
        paint::progress(
            ctx.scene,
            ctx.transform,
            bar,
            frac,
            track,
            theme.accent_primary,
        );
        chrome::edge(ctx.scene, ctx.transform, track, bar, bar.height() / 2.0);
        let pct = format!("{}%", (frac * 100.0).round() as i32);
        ctx.text.draw_text(
            ctx.scene,
            &pct,
            12.0,
            c.text_dim,
            ctx.transform * Affine::translate((x + w - 48.0, meta_y + 31.0)),
            None,
        );
        if anno > 0 {
            let label = format!("{anno} notes");
            let (lw, _) = ctx.text.measure(&label, 11.0, None);
            let badge = Rect::new(
                x + 2.0,
                meta_y + 50.0,
                x + 2.0 + f64::from(lw) + 16.0,
                meta_y + 50.0 + 18.0,
            );
            chrome::quiet_pill(
                ctx.scene,
                ctx.transform,
                theme,
                badge,
                9.0,
                c.surfaces.raised,
                false,
            );
            ctx.text.draw_text(
                ctx.scene,
                &label,
                11.0,
                c.text_dim,
                ctx.transform * Affine::translate((x + 10.0, meta_y + 53.0)),
                None,
            );
        }
        meta_y
    }

    /// The Share/Remove affordance cluster over the cover foot, shown on
    /// hover only (pushed after the whole-card hit, so they win the click
    /// where they overlap it).
    fn draw_book_card_hover_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        cell: CardCell,
        book_id: &str,
        shared: bool,
    ) {
        let theme = ctx.theme;
        let CardCell { x, y, w, cover_h } = cell;
        let sh = 26.0;
        let by = y + cover_h - sh - 8.0;
        let mut right = x + w - 10.0;
        if !shared {
            let sl = "Share";
            let (slw, _) = ctx
                .text
                .measure_weighted(sl, 12.0, None, TextWeight::Semibold);
            let sw = f64::from(slw) + 16.0 + 22.0;
            let sx = right - sw;
            let sr = Rect::new(sx, by, sx + sw, by + sh);
            ctx.fill_round(sr, sh / 2.0, theme.accent_primary);
            let share_ink = theme.on_surface(theme.accent_primary).ink;
            paint::people(
                ctx.scene,
                ctx.transform,
                Rect::new(sx + 8.0, by + 6.0, sx + 22.0, by + 20.0),
                share_ink,
                1.4,
            );
            ctx.text.draw_text_weighted(
                ctx.scene,
                sl,
                12.0,
                share_ink,
                ctx.transform * Affine::translate((sx + 26.0, by + 6.0)),
                None,
                TextWeight::Semibold,
            );
            self.push_hit(sr, Action::ShareToWorkspace(book_id.to_string()));
            right = sx - rhythm::RELATED;
        }
        // Remove — a quiet overlay disc with its derived edge and a trash glyph.
        let rr = sh / 2.0;
        let rc = Point::new(right - rr, by + rr);
        let disc = Rect::new(rc.x - rr, rc.y - rr, rc.x + rr, rc.y + rr);
        ui::surface::card(
            ctx.scene,
            ctx.transform,
            disc,
            rr,
            theme.tokens.colors.surfaces.overlay,
        );
        paint::trash(
            ctx.scene,
            ctx.transform,
            Rect::new(rc.x - 8.0, rc.y - 8.0, rc.x + 8.0, rc.y + 8.0),
            theme.tokens.colors.text_primary,
            1.5,
        );
        self.push_hit(
            Rect::new(rc.x - rr, rc.y - rr, rc.x + rr, rc.y + rr),
            Action::RemoveFromLibrary(book_id.to_string()),
        );
    }

    /// The trailing "Add books" tile: opens the host file picker.
    fn draw_add_book_card(&mut self, ctx: &mut PaintCtx<'_>, cell: CardCell) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let CardCell { x, y, w, cover_h } = cell;
        let tile = cell.cover();
        let hover = self.is_hover(tile);

        // A quiet, inviting drop surface: a raised tile under a dashed edge
        // derived from the tile itself; the accent only arrives on hover,
        // when the tile is genuinely live as a drop target (HIG §9.4).
        let fill = if hover {
            theme.bg_hover
        } else {
            c.surfaces.raised
        };
        ctx.fill_round(tile, 12.0, fill);
        let border = if hover {
            theme.accent_primary
        } else {
            ui::surface::edge_on_with(fill, ui::chrome::PANEL_BORDER_ALPHA)
        };
        dashed_round_border(
            ctx.scene,
            ctx.transform,
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
        let disc_centre_x = x + w / 2.0;
        let disc_centre_y = y + cover_h * 0.40;
        let disc_radius = 24.0;
        paint::circle(
            ctx.scene,
            ctx.transform,
            Point::new(disc_centre_x, disc_centre_y),
            disc_radius,
            theme.accent_primary.with_alpha_factor(0.16),
        );
        paint::plus(
            ctx.scene,
            ctx.transform,
            Rect::new(
                disc_centre_x - 11.0,
                disc_centre_y - 11.0,
                disc_centre_x + 11.0,
                disc_centre_y + 11.0,
            ),
            theme.accent_primary,
            2.2,
        );

        // Title + hint, centred under the mark.
        let label = "Add a book";
        let (label_w, _) = ctx
            .text
            .measure_weighted(label, 15.0, None, TextWeight::Semibold);
        ctx.text.draw_text_weighted(
            ctx.scene,
            label,
            15.0,
            c.text_primary,
            ctx.transform
                * Affine::translate((
                    disc_centre_x - f64::from(label_w) / 2.0,
                    disc_centre_y + disc_radius + 16.0,
                )),
            None,
            TextWeight::Semibold,
        );
        let hint = "Open from anywhere";
        let (hint_w, _) = ctx.text.measure(hint, 12.0, None);
        ctx.text.draw_text(
            ctx.scene,
            hint,
            12.0,
            c.text_dim,
            ctx.transform
                * Affine::translate((
                    disc_centre_x - f64::from(hint_w) / 2.0,
                    disc_centre_y + disc_radius + 38.0,
                )),
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
    /// Top of the running header's line box and the y of the rule beneath it
    /// (framed page only; zero otherwise). The content box starts below the
    /// rule, so header, rule and first line never meet at any text scale.
    header_y: f64,
    rule_y: f64,
}

/// The reader top bar's cluster sizes, measured once per frame so the
/// centre title and the right cluster lay out from the same numbers.
#[derive(Clone, Copy)]
struct ReaderTopBar {
    /// The window rect; the bar is its top `TOP_BAR`.
    r: Rect,
    /// Side of an ordinary icon button, and of the larger bookmark toggle.
    icon: f64,
    bookmark: f64,
    /// The measured "Aa" label, and the settings pill's width from it.
    aa_label: (f32, f32),
    aa_w: f64,
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
        self.running_header_h = f64::from(
            ctx.text
                .measure_weighted("Ag", running_header_px(), None, TextWeight::Semibold)
                .1,
        );
        let geom = self.reader_page_geometry(r, mode);
        let palette = self.resolve_palette(theme);

        // ---- (re)compute the chapter layout -----------------------------
        let (base_family, font_mult, font_sig, base_mono) = self.body_font();
        let layout =
            self.resolve_chapter_layout(ctx.text, &lib, &geom, font_sig, &base_family, font_mult);
        self.resolve_pending_seek_fraction(&layout, mode, &geom);
        self.cur_page_count = layout.page_count();
        self.resolve_reader_page_index(&layout, mode, &geom);

        // ---- the on-screen reading columns (panes) ----------------------
        let panes = self.compute_panes(&layout, mode, &geom);

        // ---- page surface ------------------------------------------------
        self.draw_reader_page_surface(ctx, &lib, &palette, &geom);

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
        self.comment_overlay.set_anchor(comment_box);
        self.resolve_pending_sel_edge();

        // restore the layout cache (key drives reuse next frame)
        self.layout = Some(layout);
        // restore the annotation cache before chrome reads it (the top bar's
        // bookmark state + the notes drawer both consult `self.annotations`)
        self.annotations = annos;
        self.draw_reader_footer_and_chrome(ctx, lib, &geom, sel_anchor);
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
            if mode.is_continuous() {
                self.scroll_y = layout
                    .blocks
                    .iter()
                    .find(|b| b.index == jb)
                    .map_or(0.0, |b| f64::from(b.top));
            }
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
        let frac = self.current_progress_fraction();
        self.reading_progress = frac;
        let slider_rect = Rect::new(
            geom.full_content_x,
            geom.paper_y + geom.paper_h - 30.0,
            geom.full_content_x + f64::from(geom.full_content_w),
            geom.paper_y + geom.paper_h - 6.0,
        );
        let face = SliderFace::new(slider_rect, f64::from(frac))
            .hovered(slider_rect.contains(self.cursor))
            .dragging(self.progress_dragging)
            .focused(self.semantic_focus.as_deref() == Some("books.reader.progress"));
        self.progress_slider_track = face.track_rect();
        Slider::paint(ctx.scene, ctx.transform, theme, &face);
        self.push_hit(slider_rect, Action::SeekProgress);

        // ---- chrome: top bar, floating page bubble, selection HUD, drawers -
        self.draw_reader_topbar(ctx, &lib);
        self.reader_bubble.draw(
            ctx,
            Rect::new(geom.center_x0, geom.body_y0, geom.center_x1, geom.body_y1),
            self.cursor,
            ReaderPosition {
                chapter: self.chapter,
                page: self.page,
                page_count: self.cur_page_count,
                chapter_count: nchap.max(1),
            },
        );
        // Floating selection toolbar over the page (under any overlay rail).
        self.draw_selection_hud(ctx, geom, sel_anchor);
        self.draw_rails(ctx, &lib, geom.body_y0, geom.body_y1);
        // The on-page note composer (anchored near the passage) floats over the
        // page instead of shunting note-writing off to a separate screen.
        self.draw_note_composer(ctx, sel_anchor.map(|(_, y)| y));
        // The floating comment overlay for a clicked highlight (its own composer
        // lives inside it, so the standalone note composer above stands down).
        self.comment_overlay.draw(
            ctx,
            self.rect,
            self.cursor,
            &self.annotations,
            self.hi_color,
        );
        // Reading-settings floats above everything and owns its own modal hit
        // stack; only the available reader span is supplied by the host.
        self.reading_settings.draw(
            ctx,
            self.rect,
            geom.center_x0 + 12.0,
            geom.center_x1 - 18.0,
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
        // ---- geometry of the three columns ------------------------------
        // Each rail insets the page only while it is *docked* — an overlay rail
        // floats over a full-width page behind a scrim, so it contributes no
        // inset. The eased reveal makes the inset grow/shrink smoothly as a
        // docked rail is summoned or dismissed.
        let toc_w = match self.toc_layout() {
            SplitLayout::Docked => self.toc_width() * self.toc_flap.reveal(),
            SplitLayout::Overlay => 0.0,
        };
        let notes_w = match self.notes_layout() {
            SplitLayout::Docked => self.notes_width() * self.notes_flap.reveal(),
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
        let sp = ui::tokens::spacing::Spacing::BASE;
        let (paper_x, paper_y, paper_w, paper_h, pad_x, pad_top, pad_bot, header_y, rule_y) =
            if framed {
                let paper_w = (center_w - 56.0).clamp(120.0, PAGE_MAX_W);
                let paper_x = center_x0 + (center_w - paper_w) / 2.0;
                let paper_y = body_y0 + 18.0;
                let paper_h = (body_y1 - body_y0 - 36.0).max(120.0);
                // Running header, then the rule an `s1` under its measured
                // line box, then the content an `s2` under the rule. The slot
                // follows the header's real height, so at 200 % the header
                // grows away from the rule instead of into it (rounded so 1×
                // keeps its pixel grid).
                let header_y = paper_y + f64::from(sp.s6);
                let rule_y = (header_y + self.running_header_h + f64::from(sp.s1)).round();
                let pad_top = rule_y - paper_y + f64::from(sp.s2);
                (
                    paper_x, paper_y, paper_w, paper_h, 46.0, pad_top, 46.0, header_y, rule_y,
                )
            } else {
                let paper_x = center_x0;
                let paper_y = body_y0;
                let paper_w = center_w;
                let paper_h = (body_y1 - body_y0).max(120.0);
                // Accessibility text needs a little extra horizontal breathing
                // room on an edge-to-edge phone page. The layout width follows
                // this inset, so words reflow before reaching the viewport edge.
                let pad_x = (22.0 + f64::from((self.settings.font_scale - 1.0).max(0.0)) * 11.0)
                    .clamp(22.0, 36.0);
                (
                    paper_x, paper_y, paper_w, paper_h, pad_x, 24.0, 30.0, 0.0, 0.0,
                )
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
            header_y,
            rule_y,
        }
    }

    /// (Re)compute the chapter layout if the cache key changed, or fall back
    /// to an empty layout when the book/chapter isn't available. Takes
    /// ownership of the previous cached layout via `self.layout.take()`.
    fn resolve_chapter_layout(
        &mut self,
        tr: &mut TextRenderer,
        lib: &[Arc<Book>],
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
            tr.text_scale(),
        );
        if self.layout.as_ref().map(|l| l.key) != Some(key)
            && let Some(book) = lib.get(self.book_idx)
            && let Some(ch) = book.chapters.get(self.chapter)
        {
            self.layout = Some(layout_chapter(
                tr,
                &ChapterLayoutRequest {
                    blocks: &ch.blocks,
                    content_width: geom.content_w,
                    content_height: geom.content_h,
                    settings: &self.settings,
                    chapter: self.chapter,
                    language: &book.language,
                    base_family,
                    font_multiplier: font_mult,
                    font_signature: font_sig,
                },
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
            // The page's elevation comes from the token ramp and follows the
            // page's own rounded outline (HIG §9.3), not a nudged dark slab.
            ui::surface::shadow(
                ctx.scene,
                ctx.transform,
                paper,
                8.0,
                &theme.tokens.elevation.e2,
            );
            ctx.fill_round(paper, 8.0, palette.page);
            ctx.stroke_round(paper, 8.0, palette.page_edge, 1.0);
        } else {
            // Borderless: the page fills the reading area, no card or shadow.
            ctx.fill_rect(paper, palette.page);
        }

        if geom.framed {
            if let Some(book) = lib.get(self.book_idx)
                && let Some(ch) = book.chapters.get(self.chapter)
            {
                // One line, ellipsized to the column: the slot above the
                // rule is a single line box, so a long title must not wrap
                // into the rule.
                let head = ctx.text.elide_to_width_weighted(
                    &ch.title.to_uppercase(),
                    running_header_px(),
                    geom.content_w,
                    TextWeight::Semibold,
                );
                ctx.text.draw_text_weighted(
                    ctx.scene,
                    &head,
                    running_header_px(),
                    palette.ink_dim,
                    ctx.transform * Affine::translate((geom.content_x, geom.header_y)),
                    None,
                    TextWeight::Semibold,
                );
            }
            paint::line(
                ctx.scene,
                ctx.transform,
                Point::new(geom.content_x, geom.rule_y),
                Point::new(geom.content_x + f64::from(geom.content_w), geom.rule_y),
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
                ctx.scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    1.0,
                    content_t,
                    &RoundedRect::from_rect(clip, 2.0),
                );
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
        Self::draw_pane_glyphs(ctx, frame, pane, self.rect.width() < FRAME_MIN_W);

        (bands.sel_anchor, bands.comment_box)
    }

    /// Note-comment bands and bookmark glyphs, drawn behind the text.
    fn draw_pane_note_bands(ctx: &mut PaintCtx<'_>, frame: &ReaderFrame<'_>, pane: &Pane) {
        let theme = ctx.theme;
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
                        paint::note(
                            ctx.scene,
                            ctx.transform,
                            Rect::new(gx, by0, gx + 20.0, by0 + 20.0),
                            palette.ink_dim,
                            1.6,
                        );
                    }
                    AnnoKind::Bookmark => {
                        let gx = content_x + f64::from(content_w) + 6.0;
                        paint::bookmark(
                            ctx.scene,
                            ctx.transform,
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
            ctx.fill_round(band, 3.0, palette.accent.with_alpha_factor(0.30));
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
        // overhang. Leave a small left allowance for list bullets/quote
        // ornament, but never let body glyphs escape the page on the right.
        if clip_to_narrow_page {
            let paint_bounds = Rect::new(
                content_x - 24.0,
                pane.y,
                content_x + f64::from(content_w),
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
                ctx.text.draw_text(
                    ctx.scene,
                    bullet,
                    b.size,
                    palette.ink_dim,
                    ctx.transform * Affine::translate((content_x + f64::from(b.indent) - 20.0, ly)),
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
                    let (cw, _) = ctx.text.measure_styled(
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
    fn draw_selection_hud(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        geom: &ReaderPageGeometry,
        anchor: Option<(f64, f64)>,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let (paper_x, paper_w) = (geom.paper_x, geom.paper_w);
        let (body_y0, body_y1) = (geom.body_y0, geom.body_y1);
        // Hide the picker while a note is being composed (the composer takes
        // over) or when there's no active selection/block.
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        if self.view != View::Reader || !has_sel || self.composing.is_some() {
            return;
        }
        // The shared reader HUD owns look/spacing/hover; this reader keeps the
        // page palette's accent and maps emitted actions to its own commands.
        let del_group = self.selection_highlight_group();
        // The HUD is app chrome, so it follows the OS theme end to end: the
        // Panel material and border (HIG §9.5), and the OS accent for the
        // chosen swatch ring — the reading palette stays on the page.
        let style = ui::ReaderHudStyle {
            surface: ui::chrome::panel_ground(theme),
            border: ui::chrome::panel_border(theme),
            elevation: theme.tokens.elevation.e2,
            text: c.text_primary,
            hover: theme.bg_hover,
            pill: c.surfaces.raised,
            ring: theme.accent_primary,
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
            ctx.scene,
            ctx.transform,
            ctx.text,
            &style,
            &ui::HudControls {
                swatches: &swatches,
                show_note: true,
                show_delete: del_group.is_some(),
            },
            ui::HudPlacement {
                anchor: anchor_pt,
                bounds,
            },
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

    /// Paint the contents + annotations rails: either docked (flush, page
    /// already inset) or an overlay flap over the page behind a dimming
    /// scrim. The shared [`FlapState`] kernel supplies `reveal`/`slide`.
    fn draw_rails(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        lib: &[Arc<Book>],
        body_y0: f64,
        body_y1: f64,
    ) {
        let theme = ctx.theme;
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
            // The shared scrim, at a strength that follows the reveal, so it
            // is derived per theme rather than a literal black wash.
            let a = toc_reveal.max(notes_reveal) * 0.5;
            ui::chrome::fill_scrim(ctx.scene, ctx.transform, r, a as f32, theme);
            // The scrim is a dismiss target behind the rails (recorded first so
            // the rail bodies, pushed later, win the topmost hit-test).
            self.push_hit(r, Action::DismissOverlay);
        }

        // Contents rail: docked rails sit flush at the left edge; an overlay
        // rail slides in from off-screen by its eased `slide`.
        if toc_layout == SplitLayout::Docked {
            if toc_reveal > 0.001 {
                self.draw_toc(ctx, lib, Rect::new(r.x0, body_y0, r.x0 + TOC_W, body_y1));
            }
        } else if toc_reveal > 0.001 {
            let x0 = r.x0 - TOC_W * (1.0 - self.toc_flap.slide());
            // Swallow taps on the rail body so they don't reach the scrim;
            // pushed first so the chapter-row hits land on top of it.
            let panel = Rect::new(x0, body_y0, x0 + TOC_W, body_y1);
            self.push_hit(panel, Action::Swallow);
            self.draw_toc(ctx, lib, panel);
        }

        // Annotations rail: docked at the right edge, or an overlay flap that
        // slides in from the right.
        if notes_layout == SplitLayout::Docked {
            if notes_reveal > 0.001 {
                let x0 = r.x1 - NOTES_W;
                self.draw_notes(ctx, Rect::new(x0, body_y0, x0 + NOTES_W, body_y1));
            }
        } else if notes_reveal > 0.001 {
            let x0 = r.x1 - NOTES_W * self.notes_flap.slide();
            let panel = Rect::new(x0, body_y0, x0 + NOTES_W, body_y1);
            self.push_hit(panel, Action::Swallow);
            self.draw_notes(ctx, panel);
        }
    }

    fn draw_reader_topbar(&mut self, ctx: &mut PaintCtx<'_>, lib: &[Arc<Book>]) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let r = self.rect;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        ctx.fill_rect(bar, c.surfaces.raised);
        chrome::bar_edge(
            ctx.scene,
            ctx.transform,
            c.surfaces.raised,
            r.x0,
            r.x1,
            r.y0 + TOP_BAR,
        );
        let s = 34.0;
        // The bookmark reads as the primary "save my place" affordance, so it's
        // drawn a touch larger than the other chrome icons.
        let s_bm = 40.0;
        let cy = r.y0 + (TOP_BAR - s) / 2.0;
        // The reading-settings ("Aa") pill lives in the right cluster; its
        // label is measured up front (the height already carries the
        // accessibility text scale) so the centre title reserves room for it
        // and the pill itself is sized from the same measurement.
        let (aa_label_w, aa_label_h) =
            ctx.text
                .measure_weighted("Aa", 15.0, None, TextWeight::Semibold);
        let aa_w = f64::from(aa_label_w) + 2.0 * f64::from(theme.tokens.spacing.s3);
        // One rhythm across the bar: a section step in from either edge, and
        // every control a sibling step from its neighbour.
        let mut x = r.x0 + rhythm::SECTION;
        self.icon_button(
            ctx,
            Rect::from_origin_size((x, cy), (s, s)),
            Ico::Back,
            false,
            Action::Back,
        );
        x += s + rhythm::SIBLING;
        let toc_active = self.toc_open();
        self.icon_button(
            ctx,
            Rect::from_origin_size((x, cy), (s, s)),
            Ico::Toc,
            toc_active,
            Action::ToggleToc,
        );

        let bar = ReaderTopBar {
            r,
            icon: s,
            bookmark: s_bm,
            aa_label: (aa_label_w, aa_label_h),
            aa_w,
        };
        // centre: title (+ author, when it fits). The title is clamped to the
        // gap between the left controls and the right controls and ellipsised
        // rather than allowed to collide with either cluster on a narrow window.
        self.draw_reader_topbar_title(ctx, lib, &bar, x);

        // right: reading-settings ("Aa") pill + bookmark toggle + notes toggle
        self.draw_reader_topbar_right(ctx, &bar);
    }

    /// The centre title (+ author, when it fits), clamped between the left
    /// controls and the right cluster and ellipsised rather than colliding
    /// with either on a narrow window.
    fn draw_reader_topbar_title(
        &self,
        ctx: &mut PaintCtx<'_>,
        lib: &[Arc<Book>],
        bar: &ReaderTopBar,
        x: f64,
    ) {
        let c = &ctx.theme.tokens.colors;
        let ReaderTopBar {
            r,
            icon: s,
            bookmark: s_bm,
            aa_w,
            ..
        } = *bar;
        let Some(book) = lib.get(self.book_idx) else {
            return;
        };
        let left_bound = x + s + rhythm::SIBLING;
        // Right cluster, walking in from the edge: notes icon, bookmark,
        // then the "Aa" pill — the title clears the left edge of that pill.
        // The same steps `draw_reader_topbar_right` lays the cluster out with.
        let right_bound = r.x1
            - rhythm::SECTION
            - s
            - rhythm::SIBLING
            - s_bm
            - rhythm::SIBLING
            - aa_w
            - rhythm::SIBLING;
        let avail = (right_bound - left_bound).max(40.0);

        let auth = format!("   {}", book.author);
        let (aw, _) = ctx.text.measure(&auth, 13.0, None);
        let full_title = ctx.text.elide_to_width_weighted(
            &book.title,
            16.0,
            ui::num::f32_of_f64(avail),
            TextWeight::Semibold,
        );
        let (tw, _) = ctx
            .text
            .measure_weighted(&full_title, 16.0, None, TextWeight::Semibold);
        // Only show the author if title + author both fit comfortably.
        let show_author = f64::from(tw) + f64::from(aw) <= avail && book.title == full_title;
        let total = if show_author {
            f64::from(tw) + f64::from(aw)
        } else {
            f64::from(tw)
        };
        let cx = (f64::midpoint(left_bound, right_bound) - total / 2.0).max(left_bound);
        ctx.text.draw_text_weighted(
            ctx.scene,
            &full_title,
            16.0,
            c.text_primary,
            ctx.transform * Affine::translate((cx, r.y0 + 17.0)),
            None,
            TextWeight::Semibold,
        );
        if show_author {
            ctx.text.draw_text(
                ctx.scene,
                &auth,
                13.0,
                c.text_dim,
                ctx.transform * Affine::translate((cx + f64::from(tw), r.y0 + 18.5)),
                None,
            );
        }
    }

    /// Right cluster: notes toggle, bookmark toggle, and the reading-settings
    /// ("Aa") pill — relocated off the removed bottom strip so typography
    /// stays one tap away. Opens the same type/spacing/theme popover, with
    /// the same active state + hit target.
    fn draw_reader_topbar_right(&mut self, ctx: &mut PaintCtx<'_>, bar: &ReaderTopBar) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let ReaderTopBar {
            r,
            icon: s,
            bookmark: s_bm,
            aa_label: (aa_label_w, aa_label_h),
            aa_w,
        } = *bar;
        let cy = r.y0 + (TOP_BAR - s) / 2.0;
        let bookmarked = self.annotations.iter().any(|a| {
            a.kind == AnnoKind::Bookmark
                && a.chapter == self.chapter
                && a.block == self.page_top_block
        });
        let mut rx = r.x1 - rhythm::SECTION - s;
        let notes_active = self.notes_open();
        self.icon_button(
            ctx,
            Rect::from_origin_size((rx, cy), (s, s)),
            Ico::Notes,
            notes_active,
            Action::ToggleNotes,
        );
        rx -= s_bm + rhythm::SIBLING;
        let cy_bm = r.y0 + (TOP_BAR - s_bm) / 2.0;
        self.icon_button(
            ctx,
            Rect::from_origin_size((rx, cy_bm), (s_bm, s_bm)),
            Ico::Bookmark(bookmarked),
            bookmarked,
            Action::ToggleBookmark,
        );

        rx -= rhythm::SIBLING + aa_w;
        // The pill is sized from its measured label: `s8` tall at 1× (level
        // with the icon buttons) and growing with the label at larger text,
        // capped to keep an `s2` margin inside the bar. A fixed 32 clipped the
        // 2× label against the bar's bottom edge. The cap is applied last:
        // under a large UI scale the tokens outgrow the fixed bar, and the
        // bar must win (a `clamp` with inverted bounds would panic).
        let sp = &theme.tokens.spacing;
        let aa_h = (f64::from(aa_label_h) + f64::from(sp.s3))
            .max(f64::from(sp.s8))
            .min(TOP_BAR - f64::from(sp.s2));
        let aa_y = r.y0 + (TOP_BAR - aa_h) / 2.0;
        let aa_rect = Rect::new(rx, aa_y, rx + aa_w, aa_y + aa_h);
        let aa_active = self.reading_settings.is_open();
        let aa_hover = self.is_hover(aa_rect);
        // A quiet chip at rest (raised face, derived edge); while its popover
        // is open it is "chosen" and takes the lift tier, not an accent wash.
        if aa_active || aa_hover {
            chrome::row(
                ctx.scene,
                ctx.transform,
                theme,
                aa_rect,
                aa_h / 2.0,
                SelectionState::new(aa_active, aa_hover, false),
                false,
            );
        } else {
            chrome::quiet_pill(
                ctx.scene,
                ctx.transform,
                theme,
                aa_rect,
                aa_h / 2.0,
                c.surfaces.raised,
                false,
            );
        }
        let aa_ink = c.text_primary;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "Aa",
            15.0,
            aa_ink,
            ctx.transform
                * Affine::translate((
                    rx + (aa_w - f64::from(aa_label_w)) / 2.0,
                    aa_y + (aa_h - f64::from(aa_label_h)) / 2.0,
                )),
            None,
            TextWeight::Semibold,
        );
        self.push_hit(aa_rect, Action::ToggleTypePopover);
    }

    /// `panel` is the rail, `TOC_W` wide: flush with `r.x0` when docked, or
    /// slid when it floats as an overlay flap.
    #[allow(
        clippy::too_many_lines,
        reason = "contents combines its fixed header, clipped virtual rows, and position indicator so their hit ordering stays explicit"
    )]
    fn draw_toc(&mut self, ctx: &mut PaintCtx<'_>, lib: &[Arc<Book>], panel: Rect) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let (panel_x0, y0, y1) = (panel.x0, panel.y0, panel.y1);
        // The rail is a quiet lift with a derived hairline on its page side,
        // so it reads as a surface beside the page rather than as base on base.
        chrome::rail(ctx.scene, ctx.transform, theme, panel, panel_x0 + TOC_W);
        // Rows are inset a sibling step into the rail and paint their chapter
        // number a section step further in; the caption shares that number
        // column's left edge so "CONTENTS" and "01" line up.
        let row_inset = rhythm::SIBLING;
        let number_inset = rhythm::SECTION;
        let sp = &theme.tokens.spacing;
        // The caption sits an `s4` down and the list starts an `s3` under its
        // measured line box (same rule as the annotations rail), so a 200 %
        // caption pushes the rows down instead of overlapping the first one.
        let caption_y = y0 + f64::from(sp.s4);
        let (_, cap_h) = ctx
            .text
            .measure_weighted("CONTENTS", 11.0, None, TextWeight::Semibold);
        ctx.text.draw_text_weighted(
            ctx.scene,
            "CONTENTS",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((panel_x0 + row_inset + number_inset, caption_y)),
            None,
            TextWeight::Semibold,
        );

        let Some(book) = lib.get(self.book_idx) else {
            return;
        };
        let list_y0 = caption_y + f64::from(cap_h) + f64::from(sp.s3);
        let list_y1 = y1 - 8.0;
        // Number column from the widest two-digit number; the title starts an
        // `s3` after it and wraps inside the pill. Every row is as tall as
        // its wrapped title needs (measured at the heavier active weight so
        // the height does not jump when the active chapter changes), floored
        // at the one-line pill; the pitch adds the inter-row gap.
        let (number_px, title_px) = (12.0, 13.0);
        let row_gap = toc_row_gap(theme);
        let row_floor = toc_row_floor(theme);
        let (num_w, num_h) = ctx
            .text
            .measure_weighted("00", number_px, None, TextWeight::Bold);
        let title_offset = number_inset + f64::from(num_w) + f64::from(sp.s3);
        let title_w = (TOC_W - 2.0 * row_inset - title_offset - f64::from(sp.s2)).max(40.0) as f32;
        let pill_heights: Vec<f64> = book
            .chapters
            .iter()
            .map(|ch| {
                let (_, th) = ctx.text.measure_weighted(
                    &ch.title,
                    title_px,
                    Some(title_w),
                    TextWeight::Semibold,
                );
                (f64::from(th) + 2.0 * f64::from(sp.s2)).max(row_floor)
            })
            .collect();
        let mut row_offsets = Vec::with_capacity(pill_heights.len());
        let mut content_h = 0.0;
        for h in &pill_heights {
            row_offsets.push(content_h);
            content_h += h + row_gap;
        }
        self.toc_row_pitch = pill_heights
            .first()
            .map_or(row_floor + row_gap, |h| h + row_gap);
        let visible_h = (list_y1 - list_y0).max(0.0);
        self.toc_max_scroll = (content_h - visible_h).max(0.0);
        if self.toc_scroll_to_active {
            let active_y = row_offsets.get(self.chapter).copied().unwrap_or(0.0);
            let active_h = pill_heights
                .get(self.chapter)
                .map_or(row_floor, |h| h + row_gap);
            if active_y < self.toc_scroll {
                self.toc_scroll = active_y;
            } else if active_y + active_h > self.toc_scroll + visible_h {
                self.toc_scroll = active_y + active_h - visible_h;
            }
            self.toc_scroll_to_active = false;
        }
        self.toc_scroll = self.toc_scroll.clamp(0.0, self.toc_max_scroll);

        let clip = Rect::new(panel_x0, list_y0, panel_x0 + TOC_W, list_y1);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        for (i, ch) in book.chapters.iter().enumerate() {
            let pill_h = pill_heights.get(i).copied().unwrap_or(row_floor);
            let y = list_y0 + row_offsets.get(i).copied().unwrap_or(0.0) - self.toc_scroll;
            if y + pill_h + row_gap < list_y0 || y > list_y1 {
                continue;
            }
            let row = Rect::new(
                panel_x0 + row_inset,
                y,
                panel_x0 + TOC_W - row_inset,
                y + pill_h,
            );
            let active = i == self.chapter;
            let hover = self.is_hover(row);
            // The current chapter is a lift (HIG §9.3), not an accent wash
            // with a bar: the accent stays free to mean live state.
            chrome::row(
                ctx.scene,
                ctx.transform,
                theme,
                row,
                f64::from(theme.tokens.radii.md),
                SelectionState::new(active, hover, false),
                false,
            );
            // Number and title each centre their measured box on the pill,
            // so the two share one centre line whatever the row's height.
            let num = format!("{:02}", i + 1);
            ctx.text.draw_text_weighted(
                ctx.scene,
                &num,
                number_px,
                if active { c.text_primary } else { c.text_dim },
                ctx.transform
                    * Affine::translate((
                        row.x0 + number_inset,
                        row.y0 + (pill_h - f64::from(num_h)) / 2.0,
                    )),
                None,
                TextWeight::Bold,
            );
            let title_weight = if active {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            };
            let (_, th) =
                ctx.text
                    .measure_weighted(&ch.title, title_px, Some(title_w), title_weight);
            ctx.text.draw_text_weighted(
                ctx.scene,
                &ch.title,
                title_px,
                if active { c.text_primary } else { c.text_dim },
                ctx.transform
                    * Affine::translate((
                        row.x0 + title_offset,
                        row.y0 + (pill_h - f64::from(th)) / 2.0,
                    )),
                Some(title_w),
                title_weight,
            );
            self.push_hit(row, Action::GotoChapter(i));
        }
        ctx.scene.pop_layer();
        Self::draw_rail_scrollbar(
            ctx,
            Point::new(panel_x0 + TOC_W - 9.0, list_y0),
            list_y1,
            RailScroll {
                offset: self.toc_scroll,
                max: self.toc_max_scroll,
                visible_h,
                content_h,
            },
        );
    }

    /// A deliberately quiet position indicator for a long rail.  It is not a
    /// draggable pseudo-scrollbar: wheel, touchpad and screen-reader focus
    /// remain the consistent interaction model, while the thumb preserves
    /// orientation in a large table of contents or annotation history.
    /// The track's top-left is `top` and it runs down to `y1`.
    fn draw_rail_scrollbar(ctx: &mut PaintCtx<'_>, top: Point, y1: f64, scroll: RailScroll) {
        let c = &ctx.theme.tokens.colors;
        let (x, y0) = (top.x, top.y);
        let RailScroll {
            offset: scroll,
            max: max_scroll,
            visible_h,
            content_h,
        } = scroll;
        if max_scroll <= 1.0 || content_h <= 0.0 {
            return;
        }
        let track_h = (y1 - y0).max(1.0);
        // A track shorter than the minimum thumb (a short window) is all thumb.
        let thumb_h = (track_h * visible_h / content_h).clamp(28.0_f64.min(track_h), track_h);
        let progress = (scroll / max_scroll).clamp(0.0, 1.0);
        let thumb_y = y0 + progress * (track_h - thumb_h);
        ctx.fill_round(
            Rect::new(x, thumb_y, x + RAIL_SCROLLBAR_W, thumb_y + thumb_h),
            RAIL_SCROLLBAR_W / 2.0,
            c.text_dim.with_alpha_factor(0.5),
        );
    }

    /// `panel` is the rail, `NOTES_W` wide: flush with `r.x1` when docked, or
    /// slid when it floats as an overlay flap from the right.
    fn draw_notes(&mut self, ctx: &mut PaintCtx<'_>, panel: Rect) {
        let theme = ctx.theme;
        let annos = self.annotations.clone();
        let (x0, y0, panel_x1, y1) = (panel.x0, panel.y0, panel.x1, panel.y1);
        chrome::rail(ctx.scene, ctx.transform, theme, panel, x0);

        Self::draw_notes_header(ctx, x0, y0, annos.len());
        let add_y = self.draw_notes_add_row(ctx, x0, panel_x1, y0);

        // The annotation history is a genuine scroll surface, not a clipped
        // preview.  Measure rows once to establish a stable list coordinate
        // system, then paint only rows intersecting the viewport. Cards are
        // siblings: one `s3` between them, a section step in from the rail.
        let list_y0 = add_y + NOTES_ADD_ROW_H + rhythm::SECTION;
        let list_y1 = y1 - rhythm::SIBLING;
        let card_x0 = x0 + rhythm::SECTION;
        let card_x1 = panel_x1 - rhythm::SECTION;
        let row_heights: Vec<f64> = annos
            .iter()
            .map(|annotation| {
                NoteCardText::measure(
                    ctx.text,
                    card_x1 - card_x0,
                    annotation,
                    self.composing == Some(annotation.id),
                )
                .card_height()
            })
            .collect();
        let content_h = row_heights.iter().sum::<f64>()
            + rhythm::SIBLING * annos.len().saturating_sub(1) as f64;
        let visible_h = (list_y1 - list_y0).max(0.0);
        self.notes_max_scroll = (content_h - visible_h).max(0.0);

        // An action that opens/edits an annotation should reveal that row,
        // without scanning or repainting more than this already-measured list.
        if let Some(target) = self.notes_scroll_to_annotation.take()
            && let Some(index) = annos.iter().position(|annotation| annotation.id == target)
        {
            let top = row_heights.iter().take(index).sum::<f64>() + rhythm::SIBLING * index as f64;
            let bottom = top + row_heights.get(index).copied().unwrap_or(0.0);
            if top < self.notes_scroll {
                self.notes_scroll = top;
            } else if bottom > self.notes_scroll + visible_h {
                self.notes_scroll = bottom - visible_h;
            }
        }
        self.notes_scroll = self.notes_scroll.clamp(0.0, self.notes_max_scroll);

        let clip = Rect::new(x0, list_y0, panel_x1, list_y1);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &clip,
        );
        let mut y = list_y0 - self.notes_scroll;
        for (a, height) in annos.iter().zip(&row_heights) {
            if y + *height >= list_y0 && y <= list_y1 {
                self.draw_note_card(ctx, card_x0, card_x1, y, a);
            }
            y += *height + rhythm::SIBLING;
        }
        ctx.scene.pop_layer();
        Self::draw_rail_scrollbar(
            ctx,
            Point::new(panel_x1 - 9.0, list_y0),
            list_y1,
            RailScroll {
                offset: self.notes_scroll,
                max: self.notes_max_scroll,
                visible_h,
                content_h,
            },
        );
    }

    /// "ANNOTATIONS" title + the count badge.
    fn draw_notes_header(ctx: &mut PaintCtx<'_>, x0: f64, y0: f64, count: usize) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        // The caption shares the cards' left edge (`s4` in from the rail)
        // instead of its own 22px inset, and the count badge follows the
        // measured caption rather than a fixed x that "ANNOTATIONS" overran
        // at larger text.
        let caption_x = x0 + f64::from(sp.s4);
        // Same `s4` drop as the contents rail's caption, so the two rail
        // captions sit level across the page.
        let caption_y = y0 + f64::from(sp.s4);
        let (cap_w, cap_h) =
            ctx.text
                .measure_weighted("ANNOTATIONS", 11.0, None, TextWeight::Semibold);
        ctx.text.draw_text_weighted(
            ctx.scene,
            "ANNOTATIONS",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((caption_x, caption_y)),
            None,
            TextWeight::Semibold,
        );
        let count = format!("{count}");
        let (cw, ch) = ctx.text.measure(&count, 11.0, None);
        let badge_h = f64::from(ch) + f64::from(sp.s1);
        let badge_x = caption_x + f64::from(cap_w) + f64::from(sp.s2);
        let caption_mid_y = caption_y + f64::from(cap_h) / 2.0;
        let badge = Rect::new(
            badge_x,
            caption_mid_y - badge_h / 2.0,
            badge_x + f64::from(cw) + 2.0 * f64::from(sp.s2),
            caption_mid_y + badge_h / 2.0,
        );
        chrome::quiet_pill(
            ctx.scene,
            ctx.transform,
            theme,
            badge,
            badge_h / 2.0,
            c.surfaces.raised,
            false,
        );
        ctx.text.draw_text(
            ctx.scene,
            &count,
            11.0,
            c.text_dim,
            ctx.transform
                * Affine::translate((
                    badge.x0 + f64::from(sp.s2),
                    badge.y0 + (badge.height() - f64::from(ch)) / 2.0,
                )),
            None,
        );
    }

    /// "+ Note on selection" action row. Returns its y (the list starts
    /// below it).
    fn draw_notes_add_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        x0: f64,
        panel_x1: f64,
        y0: f64,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let add_y = y0 + 42.0;
        let has_sel = self.sel.is_some_and(|s| !s.is_empty());
        let add_label = if has_sel {
            "+ Note on selection"
        } else {
            "Select text to annotate"
        };
        let add_rect = Rect::new(
            x0 + rhythm::SECTION,
            add_y,
            panel_x1 - rhythm::SECTION,
            add_y + NOTES_ADD_ROW_H,
        );
        let hover = has_sel && self.is_hover(add_rect);
        chrome::quiet_pill(
            ctx.scene,
            ctx.transform,
            theme,
            add_rect,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
            hover,
        );
        let (_, label_h) = ctx
            .text
            .measure_weighted(add_label, 12.0, None, TextWeight::Medium);
        ctx.text.draw_text_weighted(
            ctx.scene,
            add_label,
            12.0,
            if has_sel { c.text_primary } else { c.text_dim },
            ctx.transform
                * Affine::translate((
                    add_rect.x0 + rhythm::SIBLING,
                    add_rect.y0 + (add_rect.height() - f64::from(label_h)) / 2.0,
                )),
            None,
            TextWeight::Medium,
        );
        if has_sel {
            self.push_hit(add_rect, Action::AddNote);
        }
        add_y
    }

    /// Paint one measured annotation card in the virtual notes list.
    fn draw_note_card(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        card_x0: f64,
        card_x1: f64,
        y: f64,
        a: &Annotation,
    ) -> f64 {
        let composing = self.composing == Some(a.id);
        let text = NoteCardText::measure(ctx.text, card_x1 - card_x0, a, composing);
        let card_h = text.card_height();
        let card = Rect::new(card_x0, y, card_x1, y + card_h);

        self.draw_note_card_frame(ctx, card, composing);
        self.draw_note_card_header(ctx, card, a);
        Self::draw_note_card_body(ctx, card.origin(), &text);
        self.push_note_card_hits(card, a);

        y + card_h + 12.0
    }

    fn draw_note_card_frame(&self, ctx: &mut PaintCtx<'_>, card: Rect, composing: bool) {
        let theme = ctx.theme;
        let hover = self.is_hover(card);
        let radius = f64::from(theme.tokens.radii.md);
        // A raised card with its derived rest edge; hover strengthens that
        // edge to the Panel border, composing wears the focus ring.
        chrome::card(ctx.scene, ctx.transform, theme, card, radius);
        if composing {
            ctx.stroke_round(card, radius, theme.border_focus, 1.6);
        } else if hover {
            ctx.stroke_round(card, radius, ui::chrome::panel_border(theme), 1.0);
        }
    }

    /// Colour dot / bookmark glyph + kind label + chapter/paragraph locator,
    /// and the always-visible delete button.
    fn draw_note_card_header(&mut self, ctx: &mut PaintCtx<'_>, card: Rect, a: &Annotation) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let (card_x0, card_x1, y) = (card.x0, card.x1, card.y0);
        // header: colour dot + kind + location, every piece centred on the
        // header band's centre line (HIG §7: measured line boxes, not offsets).
        let cy = y + NOTE_CARD_HEADER_H / 2.0;
        let dot = Point::new(card_x0 + rhythm::SECTION, cy);
        match a.kind {
            AnnoKind::Bookmark => paint::bookmark(
                ctx.scene,
                ctx.transform,
                Rect::new(dot.x - 7.0, cy - 8.0, dot.x + 7.0, cy + 8.0),
                theme.accent_primary,
                1.5,
                true,
            ),
            _ => paint::circle(
                ctx.scene,
                ctx.transform,
                dot,
                6.0,
                ReaderPalette::highlight_swatch(a.color),
            ),
        }
        let (_, kind_h) =
            ctx.text
                .measure_weighted(a.kind.label(), 12.0, None, TextWeight::Semibold);
        ctx.text.draw_text_weighted(
            ctx.scene,
            a.kind.label(),
            12.0,
            c.text_primary,
            ctx.transform
                * Affine::translate((dot.x + 6.0 + rhythm::RELATED, cy - f64::from(kind_h) / 2.0)),
            None,
            TextWeight::Semibold,
        );
        let loc = format!("Ch {} · ¶{}", a.chapter + 1, a.block + 1);
        let (lw, loc_h) = ctx.text.measure(&loc, 11.0, None);
        // Delete is always drawn and hit-registered, not gated on hover —
        // the only way to remove an annotation, so it must reach touch/keyboard.
        // A full pointer target, a section step in from the card's edge; the
        // locator ends a related step before it.
        let del = Self::note_card_delete_rect(card_x1, y);
        ctx.text.draw_text(
            ctx.scene,
            &loc,
            11.0,
            c.text_dim,
            ctx.transform
                * Affine::translate((
                    del.x0 - rhythm::RELATED - f64::from(lw),
                    cy - f64::from(loc_h) / 2.0,
                )),
            None,
        );
        let del_hover = self.is_hover(del);
        let del_col = if del_hover {
            c.text_primary
        } else {
            c.text_dim.with_alpha_factor(0.55)
        };
        paint::close(ctx.scene, ctx.transform, del, del_col, 1.4);
        self.push_hit(pointer_target(del, theme), Action::Delete(a.id));
    }

    /// The excerpt and note text of a card whose top-left is `origin`.
    fn draw_note_card_body(ctx: &mut PaintCtx<'_>, origin: Point, text: &NoteCardText) {
        let c = &ctx.theme.tokens.colors;
        let (card_x0, y) = (origin.x, origin.y);
        // excerpt
        ctx.text.draw_text(
            ctx.scene,
            &text.excerpt,
            12.0,
            c.text_dim,
            ctx.transform * Affine::translate((card_x0 + 14.0, y + NOTE_CARD_HEADER_H)),
            Some(text.inner_w),
        );
        // note body
        if !text.note.is_empty() {
            ctx.text.draw_text(
                ctx.scene,
                &text.note,
                12.5,
                c.text_primary,
                ctx.transform
                    * Affine::translate((
                        card_x0 + 14.0,
                        y + NOTE_CARD_HEADER_H + f64::from(text.excerpt_h) + rhythm::RELATED,
                    )),
                Some(text.inner_w),
            );
        }
    }

    /// click card → jump; click again on a note → edit.
    /// The delete control's rect on an annotation card whose top is `y`: a
    /// full `NOTE_CARD_CONTROL` pointer target, a section step in from the
    /// card's right edge, level with the header's centre line.
    fn note_card_delete_rect(card_x1: f64, y: f64) -> Rect {
        let x1 = card_x1 - rhythm::SECTION;
        let y0 = y + NOTE_CARD_HEADER_H / 2.0 - NOTE_CARD_CONTROL / 2.0;
        Rect::new(x1 - NOTE_CARD_CONTROL, y0, x1, y0 + NOTE_CARD_CONTROL)
    }

    fn push_note_card_hits(&mut self, card: Rect, a: &Annotation) {
        let (card_x0, card_x1, y) = (card.x0, card.x1, card.y0);
        // The card's actionable area stops a related step short of the
        // delete control, so the two targets never share an edge.
        let right = Self::note_card_delete_rect(card_x1, y).x0 - rhythm::RELATED;
        if a.kind == AnnoKind::Note {
            // Header band jumps to the passage; the body below it (excerpt +
            // note text) begins editing. The two bands are disjoint, an `s1`
            // apart, so neither the pointer nor the accessibility tree sees
            // one target under another.
            let split = y + NOTE_CARD_HEADER_H;
            self.push_hit(
                Rect::new(card_x0, y, right, split - rhythm::TIGHT / 2.0),
                Action::JumpTo(a.id),
            );
            self.push_hit(
                Rect::new(card_x0, split + rhythm::TIGHT / 2.0, right, card.y1),
                Action::EditNote(a.id),
            );
        } else {
            // jump is the primary intent for the whole card area minus delete
            self.push_hit(Rect::new(card_x0, y, right, card.y1), Action::JumpTo(a.id));
        }
    }
}

/// An annotation card's text, measured at the card's inner width: the quoted
/// excerpt and the note (with a caret while it is being composed). The
/// virtual notes list sizes its rows from the same measurement it paints.
struct NoteCardText {
    inner_w: f32,
    excerpt: String,
    excerpt_h: f32,
    note: String,
    note_h: f32,
}

impl NoteCardText {
    fn measure(tr: &mut TextRenderer, card_w: f64, a: &Annotation, composing: bool) -> Self {
        let inner_w = (card_w - 28.0) as f32;
        let excerpt = if a.excerpt.is_empty() {
            "(no excerpt)".to_string()
        } else {
            format!("“{}”", a.excerpt)
        };
        let (_, excerpt_h) = tr.measure(&excerpt, 12.0, Some(inner_w));
        let note = if composing {
            format!("{}\u{2502}", a.note)
        } else {
            a.note.clone()
        };
        let (_, note_h) = if note.is_empty() {
            (0.0, 0.0)
        } else {
            tr.measure(&note, 12.5, Some(inner_w))
        };
        Self {
            inner_w,
            excerpt,
            excerpt_h,
            note,
            note_h,
        }
    }

    fn card_height(&self) -> f64 {
        NOTE_CARD_HEADER_H
            + f64::from(self.excerpt_h)
            + if self.note.is_empty() {
                0.0
            } else {
                f64::from(self.note_h) + 12.0
            }
            + rhythm::SECTION
    }
}

/// The settings screen's centred content column: its left edge and width.
#[derive(Clone, Copy)]
struct SettingsColumn {
    x: f64,
    w: f64,
}

/// A rail list's scroll state: the offset, the furthest reachable offset,
/// and the visible and total content heights.
#[derive(Clone, Copy)]
struct RailScroll {
    offset: f64,
    max: f64,
    visible_h: f64,
    content_h: f64,
}

// --- Settings screen: the font + colour-scheme managers ---------------------

impl BooksApp {
    fn draw_settings(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let r = self.rect;

        self.draw_settings_topbar(ctx, r);
        let tabs_y = r.y0 + TOP_BAR + 10.0;
        let tab_h = self.draw_settings_tabs(ctx, r, tabs_y);

        // Scrollable body.
        let content_top = tabs_y + tab_h + 18.0;
        let body = Rect::new(r.x0, content_top, r.x1, r.y1);
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let col_w = (r.width() - 96.0).clamp(280.0, 760.0);
        let col = SettingsColumn {
            x: r.x0 + (r.width() - col_w) / 2.0,
            w: col_w,
        };
        let start_y = content_top - self.settings_scroll;
        let end_y = match self.settings_tab {
            SettingsTab::Library => self.draw_library_settings(ctx, col, start_y),
            SettingsTab::Fonts => self.draw_fonts_manager(ctx, col, start_y),
            SettingsTab::Colours => self.draw_schemes_manager(ctx, col, start_y),
            SettingsTab::Shortcuts => self.draw_shortcuts_settings(ctx, col, start_y),
        };
        ctx.scene.pop_layer();
        let content_h = end_y - start_y;
        let visible = (r.y1 - content_top).max(0.0);
        self.settings_max_scroll = (content_h - visible + 24.0).max(0.0);
        self.settings_scroll = self.settings_scroll.clamp(0.0, self.settings_max_scroll);

        // Modal overlays.
        if self.editing_scheme.is_some() {
            self.draw_scheme_editor(ctx);
        }
        if self.font_picker {
            self.draw_font_picker(ctx);
        }
        // The colour picker floats above everything when open.
        if self.color_picker.is_open() {
            self.color_picker
                .draw(ctx.scene, ctx.transform, ctx.text, theme, self.rect);
        }
    }

    fn draw_settings_topbar(&mut self, ctx: &mut PaintCtx<'_>, r: Rect) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOP_BAR);
        ctx.fill_rect(bar, c.surfaces.raised);
        chrome::bar_edge(
            ctx.scene,
            ctx.transform,
            c.surfaces.raised,
            r.x0,
            r.x1,
            r.y0 + TOP_BAR,
        );
        let s = 34.0;
        let cy = r.y0 + (TOP_BAR - s) / 2.0;
        self.icon_button(
            ctx,
            Rect::from_origin_size((r.x0 + rhythm::SECTION, cy), (s, s)),
            Ico::Back,
            false,
            Action::CloseSettings,
        );
        ctx.text.draw_text_weighted(
            ctx.scene,
            "Settings",
            21.0,
            c.text_primary,
            ctx.transform * Affine::translate((r.x0 + 60.0, r.y0 + 15.0)),
            None,
            TextWeight::Bold,
        );
    }

    /// The underline-indicator tab bar. Wide layouts stay on one line; narrow
    /// or large-text layouts use a bounded two-by-two grid so every tab remains
    /// fully visible and touchable instead of clipping at the viewport edge.
    fn draw_settings_tabs(&mut self, ctx: &mut PaintCtx<'_>, r: Rect, tabs_y: f64) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let tab_h = 38.0;
        let left = r.x0 + 26.0;
        let right = r.x1 - 26.0;
        let mut tx = left;
        let mut ty = tabs_y;
        for (tab, label) in [
            (SettingsTab::Library, "Library"),
            (SettingsTab::Fonts, "Fonts"),
            (SettingsTab::Colours, "Colour schemes"),
            (SettingsTab::Shortcuts, "Shortcuts"),
        ] {
            let active = self.settings_tab == tab;
            let (lw, lh) = ctx
                .text
                .measure_weighted(label, 14.0, None, TextWeight::Semibold);
            let tab_w = f64::from(lw) + 24.0;
            if tx > left && tx + tab_w > right {
                tx = left;
                ty += tab_h;
            }
            let tab_rect = Rect::new(tx, ty, tx + tab_w, ty + tab_h);
            let hover = self.is_hover(tab_rect);
            let col = if active || hover {
                c.text_primary
            } else {
                c.text_dim
            };
            ctx.text.draw_text_weighted(
                ctx.scene,
                label,
                14.0,
                col,
                ctx.transform * Affine::translate((tx + 12.0, ty + (tab_h - f64::from(lh)) / 2.0)),
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
                    ty + tab_h - 3.0,
                    tx + f64::from(lw) + 16.0,
                    ty + tab_h,
                );
                ctx.fill_round(ind, 1.5, theme.accent_primary);
            }
            // The selected tab remains a member of the discoverable radio
            // group. Re-selecting it is an idempotent action.
            self.push_hit(tab_rect, Action::SetSettingsTab(tab));
            tx += tab_w + 6.0;
        }
        // Divider under the whole tab bar, derived against the base it sits on.
        let total_h = ty - tabs_y + tab_h;
        chrome::bar_edge(
            ctx.scene,
            ctx.transform,
            c.surfaces.base,
            r.x0,
            r.x1,
            tabs_y + total_h,
        );
        total_h
    }

    /// Device-local keyboard binding editor. Each row records one printable
    /// key directly; bindings are deliberately not part of the Automerge
    /// document because another reader's keyboard layout is not shared book
    /// state. Arrow/Page navigation remains available as a reliable fallback.
    #[allow(
        clippy::too_many_lines,
        reason = "the compact shortcut settings row keeps its visual, hit-target, and accessible capture state together"
    )]
    fn draw_shortcuts_settings(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
    ) -> f64 {
        let SettingsColumn { x, w } = col;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "KEYBOARD SHORTCUTS",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        ctx.text.draw_text(
            ctx.scene,
            "Choose a row, then press one printable key. These bindings stay on this device; Arrow and Page keys always navigate.",
            12.5,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            Some(w as f32),
        );
        y += 54.0;

        if let Some(error) = &self.shortcut_capture_error {
            ctx.text.draw_text_weighted(
                ctx.scene,
                error,
                12.0,
                c.semantic.danger,
                ctx.transform * Affine::translate((x, y - 24.0)),
                Some(w as f32),
                TextWeight::Medium,
            );
        }

        for shortcut in ReaderShortcut::ALL {
            let row = Rect::new(x, y, x + w, y + SETTINGS_SHORTCUT_ROW_H);
            let capturing = self.shortcut_capture == Some(shortcut);
            let hovered = self.is_hover(row);
            let radius = f64::from(theme.tokens.radii.md);
            // A raised card with its derived edge; hover lifts the fill, and a
            // capture in progress is live state, so it alone wears the accent.
            let fill = if hovered {
                theme.bg_hover
            } else {
                c.surfaces.raised
            };
            ui::surface::card(ctx.scene, ctx.transform, row, radius, fill);
            if capturing {
                ctx.stroke_round(row, radius, theme.accent_primary, 2.0);
            }
            // Label and binding pill share the row's vertical centre; both
            // centre their measured line boxes so they stay level with each
            // other (and inside the row) at any text scale.
            let (_, label_h) =
                ctx.text
                    .measure_weighted(shortcut.label(), 14.0, None, TextWeight::Semibold);
            ctx.text.draw_text_weighted(
                ctx.scene,
                shortcut.label(),
                14.0,
                c.text_primary,
                ctx.transform
                    * Affine::translate((x + 16.0, y + (row.height() - f64::from(label_h)) / 2.0)),
                None,
                TextWeight::Semibold,
            );
            let binding = if capturing {
                "Press a key…".to_string()
            } else {
                self.local_library
                    .shortcut(shortcut.id(), shortcut.default_key())
                    .to_uppercase()
            };
            let (binding_w, binding_h) =
                ctx.text
                    .measure_weighted(&binding, 13.0, None, TextWeight::Semibold);
            let pill_w = (f64::from(binding_w) + 24.0).max(42.0);
            let pill_h = 32.0;
            let pill = Rect::new(
                x + w - pill_w - rhythm::SECTION,
                y + (row.height() - pill_h) / 2.0,
                x + w - rhythm::SECTION,
                y + f64::midpoint(row.height(), pill_h),
            );
            if capturing {
                ctx.fill_round(pill, 7.0, theme.accent_primary);
            } else {
                chrome::card_on_raised(ctx.scene, ctx.transform, theme, pill, 7.0);
            }
            ctx.text.draw_text_weighted(
                ctx.scene,
                &binding,
                13.0,
                if capturing {
                    theme.on_surface(theme.accent_primary).ink
                } else {
                    c.text_primary
                },
                ctx.transform
                    * Affine::translate((
                        pill.x0 + (pill.width() - f64::from(binding_w)) / 2.0,
                        pill.y0 + (pill.height() - f64::from(binding_h)) / 2.0,
                    )),
                None,
                TextWeight::Semibold,
            );
            self.push_hit(row, Action::BeginShortcutCapture(shortcut));
            y += SETTINGS_SHORTCUT_ROW_H + rhythm::SIBLING;
        }
        y
    }

    /// The Library settings pane: manage the folders watched for new books,
    /// the secondary way in ("Open a book" stays primary).
    fn draw_library_settings(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
    ) -> f64 {
        y = Self::draw_library_settings_header(ctx, col, y);

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
            return self.draw_library_settings_empty(ctx, col, y);
        }

        self.draw_watched_folder_rows(ctx, col, y, &folders)
    }

    /// "WATCHED FOLDERS" title + the explanatory blurb. Returns the y just
    /// below it.
    fn draw_library_settings_header(
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
    ) -> f64 {
        let SettingsColumn { x, w } = col;
        let c = &ctx.theme.tokens.colors;
        // A short read on where books come from — no leaked internal paths, no
        // "magic folder": the primary way in is opening a book.
        ctx.text.draw_text_weighted(
            ctx.scene,
            "WATCHED FOLDERS",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        ctx.text.draw_text(
            ctx.scene,
            "Turn a folder on to add its books to your library automatically, and to keep noticing new ones. Everything you open is added to your library either way.",
            12.5,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            Some(w as f32),
        );
        y + 40.0
    }

    /// The empty state: an explanation card + the primary "Open a book"
    /// action (no folder picker — the watch list grows from books you open).
    fn draw_library_settings_empty(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
    ) -> f64 {
        let SettingsColumn { x, w } = col;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let radii_md = f64::from(theme.tokens.radii.md);
        let card = Rect::new(x, y, x + w, y + 84.0);
        chrome::card(ctx.scene, ctx.transform, theme, card, radii_md);
        ctx.text.draw_text_weighted(
            ctx.scene,
            "No folders yet",
            14.0,
            c.text_primary,
            ctx.transform * Affine::translate((x + 18.0, y + 18.0)),
            None,
            TextWeight::Semibold,
        );
        ctx.text.draw_text(
            ctx.scene,
            "Open a book from a folder and it will appear here, ready to watch.",
            12.5,
            c.text_dim,
            ctx.transform * Affine::translate((x + 18.0, y + 42.0)),
            Some((w - 36.0) as f32),
        );
        y += 84.0 + rhythm::SECTION;
        let bw = 150.0;
        self.accent_button(
            ctx,
            Rect::new(x, y, x + bw, y + 40.0),
            "Open a book…",
            true,
            Action::OpenViaPortal,
        );
        y + 40.0
    }

    /// One row per candidate folder: name, resolved path, and its watch
    /// toggle. Returns the y just below the last row.
    fn draw_watched_folder_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
        folders: &[PathBuf],
    ) -> f64 {
        let SettingsColumn { x, w } = col;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let radii_md = f64::from(theme.tokens.radii.md);
        let home = tilo_paths::home().map(|home| home.to_string_lossy().into_owned());
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
                Some(h) if !h.is_empty() => match s.strip_prefix(h.as_str()) {
                    Some(rest) => format!("~{rest}"),
                    None => s,
                },
                _ => s,
            }
        };

        for folder in folders {
            let row_h = 60.0;
            let card = Rect::new(x, y, x + w, y + row_h);
            chrome::card(ctx.scene, ctx.transform, theme, card, radii_md);

            let watched = self.watched_folders.iter().any(|f| f == folder);
            let name = folder
                .file_name()
                .map_or_else(|| pretty(folder), |n| n.to_string_lossy().to_string());
            ctx.text.draw_text_weighted(
                ctx.scene,
                &name,
                15.0,
                c.text_primary,
                ctx.transform * Affine::translate((x + 18.0, y + 12.0)),
                Some((w - 100.0) as f32),
                TextWeight::Semibold,
            );
            ctx.text.draw_text(
                ctx.scene,
                &pretty(folder),
                12.0,
                c.text_dim,
                ctx.transform * Affine::translate((x + 18.0, y + 34.0)),
                Some((w - 100.0) as f32),
            );

            // Watch toggle at the row's right edge.
            let tog_w = 44.0;
            let tog_h = 24.0;
            self.switch(
                ctx,
                Rect::new(
                    x + w - 18.0 - tog_w,
                    y + (row_h - tog_h) / 2.0,
                    x + w - 18.0,
                    y + f64::midpoint(row_h, tog_h),
                ),
                watched,
                Action::ToggleWatchFolder(folder.clone()),
            );
            y += row_h + rhythm::SIBLING;
        }

        y
    }

    fn draw_fonts_manager(&mut self, ctx: &mut PaintCtx<'_>, col: SettingsColumn, y: f64) -> f64 {
        let x = col.x;
        let fonts = self.fonts.clone();
        let default_id = if self.default_font_id.is_empty() {
            fonts.first().map(|f| f.id.clone()).unwrap_or_default()
        } else {
            self.default_font_id.clone()
        };

        let current = fonts.iter().find(|f| f.id == default_id);
        let (mut y, dropdown) = self.draw_default_font_dropdown(ctx, col, y, current);

        y = Self::draw_font_rows_header(ctx, col, y);
        for f in &fonts {
            y = self.draw_font_row(ctx, col, y, f);
        }
        y = self.draw_add_font_button(ctx, x, y);

        // The default-font dropdown menu (drawn last so it floats over the rows).
        if self.default_font_menu {
            self.draw_default_font_menu(ctx, dropdown, &fonts, &default_id);
        }
        y
    }

    /// The default-font dropdown control: label + box showing the current
    /// default's name in its own face. Returns `(y_below, dropdown)` — the
    /// dropdown's rect anchors the flyout menu.
    fn draw_default_font_dropdown(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        mut y: f64,
        current: Option<&FontEntry>,
    ) -> (f64, Rect) {
        let SettingsColumn { x, w } = col;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let radii_md = f64::from(theme.tokens.radii.md);
        ctx.text.draw_text_weighted(
            ctx.scene,
            "DEFAULT FONT",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        let dd_h = 46.0;
        let dd = Rect::new(x, y, x + w, y + dd_h);
        // A dropdown field rests like a Neutral button (raised, derived edge)
        // and wears the focus ring while its list is open (HIG §9.5).
        chrome::card(ctx.scene, ctx.transform, theme, dd, radii_md);
        if self.default_font_menu {
            ctx.stroke_round(dd, radii_md, theme.border_focus, 2.0);
        }
        if let Some(f) = current {
            // Draw the current default's name in its own face.
            ctx.text.draw_style(
                ctx.scene,
                &f.name,
                c.text_primary,
                ctx.transform * Affine::translate((x + 16.0, y + dd_h / 2.0 - 10.0)),
                None,
                &TextStyle::new(16.0)
                    .with_weight(TextWeight::Semibold)
                    .with_family(font_choice(&f.kind)),
            );
        }
        self.icon_button(
            ctx,
            Rect::from_origin_size((x + w - 34.0, y + dd_h / 2.0 - 14.0), (28.0, 28.0)),
            Ico::ChevDown,
            false,
            Action::ToggleDefaultFontMenu,
        );
        self.push_hit(dd, Action::ToggleDefaultFontMenu);
        // Before the next group: one `s6`, the same step every section on
        // this screen uses.
        y += dd_h + rhythm::GROUP;
        (y, dd)
    }

    /// "READING FONTS" section title + description. Returns the y just
    /// below it, where the row list starts.
    fn draw_font_rows_header(ctx: &mut PaintCtx<'_>, col: SettingsColumn, mut y: f64) -> f64 {
        let SettingsColumn { x, w } = col;
        let c = &ctx.theme.tokens.colors;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "READING FONTS",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 18.0;
        ctx.text.draw_text(ctx.scene, "Each row previews its own face — tap a name to rename, toggle whether it appears in the quick settings, and tune its size.", 12.5, c.text_dim, ctx.transform * Affine::translate((x, y)), Some(w as f32));
        y + 34.0
    }

    /// One font row: name (or its rename editor), kind label, and the right
    /// control cluster. Returns the y just below it.
    fn draw_font_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
        f: &FontEntry,
    ) -> f64 {
        let SettingsColumn { x, w } = col;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let radii_md = f64::from(theme.tokens.radii.md);
        let row_h = 64.0;
        let cy = y + row_h / 2.0;
        let card = Rect::new(x, y, x + w, y + row_h);
        chrome::card(ctx.scene, ctx.transform, theme, card, radii_md);

        self.draw_font_row_name(ctx, col, y, f);

        let kind = match &f.kind {
            FontKind::Default => "Default face".to_string(),
            FontKind::Serif => "Serif book face".to_string(),
            FontKind::Mono => "Monospace".to_string(),
            FontKind::Named(n) => format!("System · {n}"),
        };
        ctx.text.draw_text(
            ctx.scene,
            &kind,
            12.0,
            c.text_dim,
            ctx.transform * Affine::translate((x + 20.0, y + 38.0)),
            None,
        );

        self.draw_font_row_controls(ctx, col, cy, f);

        y + row_h + rhythm::SIBLING
    }

    /// Name rendered in its own face. When renaming, the real inline editor
    /// (a `ui::EditableLabel` over a `TextInput`) takes over the slot;
    /// otherwise a pencil affordance invites a tap to rename.
    fn draw_font_row_name(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
        f: &FontEntry,
    ) {
        let x = col.x;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let name_x = x + 20.0;
        let renaming_this =
            matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Font(f.id.clone()));
        let fam = font_choice(&f.kind);
        if renaming_this {
            // Editor spans from the name to an `s3` gap before the right
            // control cluster — the same cluster edge `draw_font_row_controls`
            // lays out from, so the field can never run under the toggle the
            // way a fixed 210px reservation did. Its left edge backs off by
            // the shared field's own text inset so the edited name starts
            // exactly where the static name was painted.
            let controls_x0 = Self::font_row_controls_x0(ctx.text, theme, col, f);
            let field_x1 = (controls_x0 - f64::from(theme.tokens.spacing.s3)).max(name_x + 140.0);
            let field_rect = Rect::new(
                name_x - f64::from(theme.padding_standard),
                y + 8.0,
                field_x1,
                y + 8.0 + 30.0,
            );
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field_rect);
                rn.field.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
        } else {
            ctx.text.draw_style(
                ctx.scene,
                &f.name,
                c.text_primary,
                ctx.transform * Affine::translate((name_x, y + 12.0)),
                None,
                &TextStyle::new(16.0)
                    .with_weight(TextWeight::Semibold)
                    .with_family(fam.clone()),
            );
            let (nw, _) =
                ctx.text
                    .measure_font(&f.name, 16.0, None, TextWeight::Semibold, false, &fam);
            let pencil = Rect::new(
                name_x + f64::from(nw) + 6.0,
                y + 12.0,
                name_x + f64::from(nw) + 26.0,
                y + 32.0,
            );
            paint::pencil(
                ctx.scene,
                ctx.transform,
                pencil,
                c.text_dim.with_alpha_factor(0.5),
                1.3,
            );
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
    fn draw_font_row_controls(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        cy: f64,
        f: &FontEntry,
    ) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let del_col = Self::font_row_del_col(col);
        if matches!(f.kind, FontKind::Named(_)) {
            let del = Rect::new(del_col, cy - 12.0, del_col + 24.0, cy + 12.0);
            let col = if self.is_hover(del) {
                c.semantic.danger
            } else {
                c.text_dim.with_alpha_factor(0.6)
            };
            paint::close(ctx.scene, ctx.transform, del, col, 1.4);
            self.push_hit(del, Action::RemoveFont(f.id.clone()));
        }
        let field = NumberField::FontMult(f.id.clone());
        let size_right = Self::font_row_size_right(col);
        self.size_stepper(
            ctx,
            Point::new(size_right, cy),
            f64::from(f.size_mult),
            &field,
        );
        // Enable/disable toggle (= whether it appears in quick settings). Its
        // left edge is the one cluster edge the rename editor also stops
        // before, derived once for both.
        let tog_h = 22.0;
        let tog_x = Self::font_row_controls_x0(ctx.text, theme, col, f);
        self.switch(
            ctx,
            Rect::new(
                tog_x,
                cy - tog_h / 2.0,
                tog_x + FONT_ROW_TOGGLE_W,
                cy + tog_h / 2.0,
            ),
            f.shown,
            Action::ToggleFontShown(f.id.clone()),
        );
    }

    /// x of a font row's delete column (custom fonts only; built-ins reserve
    /// the column so every row's stepper lines up).
    fn font_row_del_col(col: SettingsColumn) -> f64 {
        col.x + col.w - 16.0 - 24.0
    }

    /// Right edge of a font row's size stepper.
    fn font_row_size_right(col: SettingsColumn) -> f64 {
        Self::font_row_del_col(col) - 12.0
    }

    /// Left edge of a font row's whole right-hand control cluster (the
    /// visibility toggle), derived from the same constants and the same
    /// measured stepper width `draw_font_row_controls` lays out with. The
    /// rename editor ends before this edge.
    fn font_row_controls_x0(
        tr: &mut TextRenderer,
        theme: &Theme,
        col: SettingsColumn,
        f: &FontEntry,
    ) -> f64 {
        let stepper_w = Self::size_stepper_width(tr, theme, &NumberField::FontMult(f.id.clone()));
        Self::font_row_size_right(col) - stepper_w - 16.0 - FONT_ROW_TOGGLE_W
    }

    /// "+ Add a system font" pill. Returns the y just below it.
    fn draw_add_font_button(&mut self, ctx: &mut PaintCtx<'_>, x: f64, mut y: f64) -> f64 {
        let theme = ctx.theme;
        let add = "Add a system font";
        let aw = f64::from(
            ctx.text
                .measure_weighted(add, 13.0, None, TextWeight::Medium)
                .0,
        ) + 32.0
            + f64::from(theme.tokens.metrics.icon_sm)
            + f64::from(theme.tokens.metrics.icon_label_gap);
        let arect = Rect::new(x, y, x + aw, y + 36.0);
        // The shared Neutral face: raised, derived edge, hover from the theme.
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(arect, add)
                .icon(ui::icons::Icon::Plus)
                .radius(18.0)
                .text_size(13.0)
                .hovered(self.is_hover(arect)),
        );
        self.push_hit(arect, Action::OpenFontPicker);
        y += 36.0 + rhythm::RELATED;
        y
    }

    /// The default-font dropdown list, floating under the `anchor` control.
    fn draw_default_font_menu(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        anchor: Rect,
        fonts: &[FontEntry],
        default_id: &str,
    ) {
        let (x, top) = (anchor.x0, anchor.y1 + 4.0);
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        // Menu rows are `MENU_ROW_H` pitches with a related step between
        // their faces and a related step of panel padding around them.
        let row_h = MENU_ROW_H + rhythm::RELATED;
        let menu_h = fonts.len() as f64 * row_h + rhythm::RELATED;
        let menu = Rect::new(x, top, anchor.x1, top + menu_h);
        chrome::panel(ctx.scene, ctx.transform, theme, menu);
        self.push_hit(menu, Action::Swallow);
        let mut my = top + rhythm::RELATED;
        for f in fonts {
            let row = Rect::new(
                x + rhythm::RELATED,
                my,
                anchor.x1 - rhythm::RELATED,
                my + MENU_ROW_H,
            );
            let sel = f.id == default_id;
            // The current default is a lift (HIG §9.3), never an accent wash.
            chrome::row(
                ctx.scene,
                ctx.transform,
                theme,
                row,
                f64::from(theme.tokens.radii.md),
                SelectionState::new(sel, self.is_hover(row), false),
                false,
            );
            ctx.text.draw_style(
                ctx.scene,
                &f.name,
                c.text_primary,
                ctx.transform * Affine::translate((x + 18.0, my + 9.0)),
                None,
                &TextStyle::new(15.0)
                    .with_weight(TextWeight::Medium)
                    .with_family(font_choice(&f.kind)),
            );
            self.push_hit(row, Action::SetDefaultFont(f.id.clone()));
            my += row_h;
        }
    }

    fn draw_schemes_manager(&mut self, ctx: &mut PaintCtx<'_>, col: SettingsColumn, y: f64) -> f64 {
        let x = col.x;
        let mut y = Self::draw_schemes_header(ctx, col, y);
        y = self.draw_new_scheme_button(ctx, x, y);

        let schemes = self.schemes.clone();
        for s in &schemes {
            y = self.draw_scheme_row(ctx, col, y, s);
        }
        y + 8.0
    }

    /// "COLOUR SCHEMES" section title + description. Returns the y just
    /// below it.
    fn draw_schemes_header(ctx: &mut PaintCtx<'_>, col: SettingsColumn, mut y: f64) -> f64 {
        let SettingsColumn { x, w } = col;
        let c = &ctx.theme.tokens.colors;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "COLOUR SCHEMES",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((x, y)),
            None,
            TextWeight::Semibold,
        );
        y += 20.0;
        ctx.text.draw_text(ctx.scene, "A scheme's colours are all edited through the colour picker. Toggle a scheme off to hide it from the quick settings.", 12.5, c.text_dim, ctx.transform * Affine::translate((x, y)), Some(w as f32));
        y + 40.0
    }

    /// A single button opens the colour picker to choose a background; the
    /// scheme is then derived from it. Returns the y just below it.
    fn draw_new_scheme_button(&mut self, ctx: &mut PaintCtx<'_>, x: f64, y: f64) -> f64 {
        let theme = ctx.theme;
        let new_label = "＋  New scheme from a colour…";
        let nw = f64::from(
            ctx.text
                .measure_weighted(new_label, 13.0, None, TextWeight::Semibold)
                .0,
        ) + 34.0;
        let nrect = Rect::new(x, y, x + nw, y + 40.0);
        // The pane's one primary action wears the shared Primary face.
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(nrect, new_label)
                .primary()
                .radius(20.0)
                .text_size(13.0)
                .weight(TextWeight::Semibold)
                .hovered(self.is_hover(nrect)),
        );
        self.push_hit(nrect, Action::NewSchemeViaPicker);
        y + 40.0 + rhythm::GROUP
    }

    /// One colour-scheme row: swatches, name/status, and the right action
    /// cluster. Returns the y just below it.
    fn draw_scheme_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        col: SettingsColumn,
        y: f64,
        s: &ColorScheme,
    ) -> f64 {
        let SettingsColumn { x, w } = col;
        let theme = ctx.theme;
        let radii_md = f64::from(theme.tokens.radii.md);
        let row_h = 68.0;
        let cy = y + row_h / 2.0;
        let card = Rect::new(x, y, x + w, y + row_h);
        let selected = if s.builtin {
            self.settings.scheme_id.is_empty() && self.settings.theme == ReadTheme::from_str(&s.id)
        } else {
            self.settings.scheme_id == s.id
        };
        // The scheme in use is "chosen", so it is the lifted tier over quiet
        // floored siblings (HIG §9.3): the accent stays free for live state.
        chrome::row(
            ctx.scene,
            ctx.transform,
            theme,
            card,
            radii_md,
            SelectionState::active(selected),
            true,
        );

        // For a custom scheme each swatch opens the colour picker on that
        // slot; a built-in's swatches just apply the scheme.
        let swx = self.draw_scheme_swatches(ctx, x, cy, s);

        // Name + status. When renaming, the inline editor fills the name
        // slot (drawn below, once its right bound is known).
        let name_x = swx + 16.0;
        let renaming_this = self.renaming_scheme(&s.id);
        Self::draw_scheme_name_and_status(ctx, Point::new(name_x, y), selected, renaming_this, s);

        let rx = self.draw_scheme_actions(ctx, Point::new(x + w, cy), s);

        self.draw_scheme_name_interaction(ctx, Rect::new(name_x, y, rx, y + row_h), s);

        y + row_h + rhythm::SIBLING
    }

    /// Whether the inline rename editor is open on scheme `id`.
    fn renaming_scheme(&self, id: &str) -> bool {
        matches!(&self.renaming, Some(rn) if rn.target == RenameTarget::Scheme(id.to_string()))
    }

    /// What activating a scheme row does: a built-in sets its reading
    /// theme, a custom scheme is applied by id.
    fn scheme_use_action(s: &ColorScheme) -> Action {
        if s.builtin {
            Action::SetReadTheme(ReadTheme::from_str(&s.id))
        } else {
            Action::SetScheme(s.id.clone())
        }
    }

    /// Colour swatches (page / ink / accent). Returns the x just past
    /// them, where the name column starts.
    fn draw_scheme_swatches(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        x: f64,
        cy: f64,
        s: &ColorScheme,
    ) -> f64 {
        let theme = ctx.theme;
        let mut swx = x + rhythm::SECTION;
        for (slot, col) in [s.page, s.ink, s.accent].into_iter().enumerate() {
            let srr = Rect::new(swx, cy - 12.0, swx + 24.0, cy + 12.0);
            let fill = Color::from_rgb8(col[0], col[1], col[2]);
            ctx.fill_round(srr, 6.0, fill);
            // Each swatch edge derives from the swatch itself, so a page-white
            // or ink-black chip keeps a visible outline on every preset.
            let edith = !s.builtin && self.is_hover(srr);
            if edith {
                ctx.stroke_round(srr, 6.0, theme.border_focus, 2.0);
            } else {
                chrome::edge(ctx.scene, ctx.transform, fill, srr, 6.0);
            }
            if !s.builtin {
                self.push_hit(srr, Action::PickSchemeColor(s.id.clone(), slot));
            }
            swx += 24.0 + rhythm::RELATED;
        }
        swx
    }

    /// Scheme name (skipped while its inline rename editor is active) and
    /// the Built-in/In use/Custom status label.
    fn draw_scheme_name_and_status(
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        selected: bool,
        renaming_this: bool,
        s: &ColorScheme,
    ) {
        let (name_x, y) = (origin.x, origin.y);
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        if !renaming_this {
            ctx.text.draw_text_weighted(
                ctx.scene,
                &s.name,
                15.0,
                c.text_primary,
                ctx.transform * Affine::translate((name_x, y + 14.0)),
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
        ctx.text.draw_text(
            ctx.scene,
            sub,
            12.0,
            if selected {
                theme.accent_primary
            } else {
                c.text_dim
            },
            ctx.transform * Affine::translate((name_x, y + 38.0)),
            None,
        );
    }

    /// Right cluster, laid out right-to-left: enable switch, then the
    /// actions (Duplicate / Edit / delete — Edit and delete only for
    /// custom schemes), ending at the card's `right_mid`. Returns the x
    /// just left of the cluster.
    fn draw_scheme_actions(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        right_mid: Point,
        s: &ColorScheme,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let cy = right_mid.y;
        // The cluster is one row of siblings: a section step in from the
        // card's edge, then a sibling step between every neighbour.
        let mut rx = right_mid.x - rhythm::SECTION;
        let tog_w = 40.0;
        let tog_h = 22.0;
        rx -= tog_w;
        self.switch(
            ctx,
            Rect::new(rx, cy - tog_h / 2.0, rx + tog_w, cy + tog_h / 2.0),
            s.enabled,
            Action::ToggleSchemeEnabled(s.id.clone()),
        );
        rx -= rhythm::SIBLING;
        if !s.builtin {
            rx -= 24.0;
            let del = Rect::new(rx, cy - 12.0, rx + 24.0, cy + 12.0);
            let dcolh = if self.is_hover(del) {
                c.semantic.danger
            } else {
                c.text_dim.with_alpha_factor(0.6)
            };
            paint::close(ctx.scene, ctx.transform, del, dcolh, 1.4);
            self.push_hit(del, Action::DeleteScheme(s.id.clone()));
            rx -= rhythm::SIBLING;
            let ew = f64::from(
                ctx.text
                    .measure_weighted("Edit", 12.0, None, TextWeight::Medium)
                    .0,
            ) + 24.0;
            rx -= ew;
            self.pill(
                ctx,
                Point::new(rx, cy - 13.0),
                26.0,
                "Edit",
                12.0,
                Action::EditScheme(s.id.clone()),
            );
            rx -= rhythm::SIBLING;
        }
        let dw = f64::from(
            ctx.text
                .measure_weighted("Duplicate", 12.0, None, TextWeight::Medium)
                .0,
        ) + 24.0;
        rx -= dw;
        self.pill(
            ctx,
            Point::new(rx, cy - 13.0),
            26.0,
            "Duplicate",
            12.0,
            Action::DuplicateScheme(s.id.clone()),
        );
        rx
    }

    /// Name click: rename (custom) / apply (built-in), bounded before the
    /// right cluster. `slot` spans the row from the name's left edge to the
    /// cluster. When renaming, the inline editor fills the slot.
    fn draw_scheme_name_interaction(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        slot: Rect,
        s: &ColorScheme,
    ) {
        let theme = ctx.theme;
        let (name_x, y) = (slot.x0, slot.y0);
        let name_end = slot.x1 - rhythm::SIBLING;
        let use_action = Self::scheme_use_action(s);
        if self.renaming_scheme(&s.id) {
            let field_x1 = name_end.max(name_x + 120.0);
            // Back off by the shared field's text inset so the edited name
            // starts exactly where the static name is painted.
            let field_rect = Rect::new(
                name_x - f64::from(theme.padding_standard),
                y + 8.0,
                field_x1,
                y + 8.0 + 28.0,
            );
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field_rect);
                rn.field.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
        } else if name_end > name_x {
            if s.builtin {
                self.push_hit(
                    Rect::new(name_x, y + 8.0, name_end, slot.y1 - 8.0),
                    use_action,
                );
            } else {
                // The name line renames; the status line under it applies the
                // scheme. The two targets sit an `s1` apart so neither the
                // pointer nor the accessibility tree sees them share an edge.
                let split = y + 34.0;
                self.push_hit(
                    Rect::new(name_x, y + 8.0, name_end, split - rhythm::TIGHT / 2.0),
                    Action::BeginRenameScheme(s.id.clone()),
                );
                self.push_hit(
                    Rect::new(name_x, split + rhythm::TIGHT / 2.0, name_end, slot.y1 - 8.0),
                    use_action,
                );
            }
        }
    }

    fn draw_scheme_editor(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let Some(id) = self.editing_scheme.clone() else {
            return;
        };
        let Some(scheme) = self.schemes.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        let r = self.rect;
        chrome::sheet_scrim(ctx.scene, ctx.transform, theme, r);
        self.push_hit(r, Action::CloseSchemeEditor);

        let pw = 440.0_f64.min(r.width() - 40.0);
        let ph = 372.0_f64.min(r.height() - 40.0);
        let px0 = r.x0 + (r.width() - pw) / 2.0;
        let py0 = r.y0 + (r.height() - ph) / 2.0;
        let panel = Rect::new(px0, py0, px0 + pw, py0 + ph);
        chrome::panel(ctx.scene, ctx.transform, theme, panel);
        self.push_hit(panel, Action::Swallow);

        let ix0 = px0 + rhythm::GROUP;
        let ix1 = px0 + pw - rhythm::GROUP;
        let y = py0 + rhythm::GROUP;

        let y = self.draw_scheme_editor_title(ctx, ix0, ix1, y, &scheme);
        let y = self.draw_scheme_editor_swatches(ctx, ix0, ix1, y, &scheme);
        let y = Self::draw_scheme_editor_preview(ctx, ix0, ix1, y, &scheme);
        self.draw_scheme_editor_done_button(ctx, ix1, y);
    }

    /// "EDIT SCHEME" title + the scheme name (or its inline rename editor).
    /// Returns the y just below it.
    fn draw_scheme_editor_title(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        scheme: &ColorScheme,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "EDIT SCHEME",
            11.0,
            c.text_dim,
            ctx.transform * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 18.0;
        if self.renaming_scheme(&scheme.id) {
            let field_rect = Rect::new(ix0 - 6.0, y - 4.0, ix1 - 60.0, y + 30.0);
            if let Some(rn) = self.renaming.as_mut() {
                rn.field.apply_layout_external(field_rect);
                rn.field.draw(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
        } else {
            ctx.text.draw_text_weighted(
                ctx.scene,
                &scheme.name,
                20.0,
                c.text_primary,
                ctx.transform * Affine::translate((ix0, y)),
                None,
                TextWeight::Bold,
            );
            self.push_hit(
                Rect::new(ix0, y - 2.0, ix1 - 60.0, y + 26.0),
                Action::BeginRenameScheme(scheme.id.clone()),
            );
        }
        y + 40.0
    }

    /// "COLOURS" section: one swatch per scheme slot, each opening the
    /// colour picker. Returns the y just below it.
    fn draw_scheme_editor_swatches(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        scheme: &ColorScheme,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "COLOURS · tap a swatch to open the picker",
            10.0,
            c.text_dim,
            ctx.transform * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 18.0;
        let slots = scheme.slots();
        let slot_w = (ix1 - ix0 - 2.0 * rhythm::SECTION) / 3.0;
        let mut sxx = ix0;
        for (i, (label, col)) in slots.iter().enumerate() {
            let sr = Rect::new(sxx, y, sxx + slot_w, y + 60.0);
            let fill = Color::from_rgb8(col[0], col[1], col[2]);
            ctx.fill_round(sr, 10.0, fill);
            // The swatch edge derives from the swatch, so the page-white
            // tile keeps its outline on a light panel.
            if self.is_hover(sr) {
                ctx.stroke_round(sr, 10.0, theme.border_focus, 2.0);
            } else {
                chrome::edge(ctx.scene, ctx.transform, fill, sr, 10.0);
            }
            self.push_hit(sr, Action::PickSchemeColor(scheme.id.clone(), i));
            let (lw, _) = ctx.text.measure(label, 12.0, None);
            ctx.text.draw_text(
                ctx.scene,
                label,
                12.0,
                c.text_dim,
                ctx.transform
                    * Affine::translate((
                        sxx + (slot_w - f64::from(lw)) / 2.0,
                        y + 60.0 + rhythm::RELATED,
                    )),
                None,
            );
            sxx += slot_w + rhythm::SECTION;
        }
        y + 60.0 + 32.0
    }

    /// "PREVIEW" section: a mock reading card rendered in the scheme's own
    /// palette. Returns the y just below it.
    fn draw_scheme_editor_preview(
        ctx: &mut PaintCtx<'_>,
        ix0: f64,
        ix1: f64,
        mut y: f64,
        scheme: &ColorScheme,
    ) -> f64 {
        let c = &ctx.theme.tokens.colors;
        ctx.text.draw_text_weighted(
            ctx.scene,
            "PREVIEW",
            10.0,
            c.text_dim,
            ctx.transform * Affine::translate((ix0, y)),
            None,
            TextWeight::Semibold,
        );
        y += 16.0;
        let pal = ReaderPalette::from_scheme(scheme);
        let prev = Rect::new(ix0, y, ix1, y + 72.0);
        // The preview is a page in the scheme's own palette, seated on the
        // panel by an edge derived from that page colour.
        ctx.fill_round(prev, 8.0, pal.page);
        chrome::edge(ctx.scene, ctx.transform, pal.page, prev, 8.0);
        ctx.text.draw_text_weighted(
            ctx.scene,
            "The quick brown fox",
            15.0,
            pal.ink,
            ctx.transform * Affine::translate((ix0 + 14.0, y + 12.0)),
            None,
            TextWeight::Semibold,
        );
        ctx.text.draw_text(
            ctx.scene,
            "jumps over the lazy dog.",
            13.0,
            pal.ink_dim,
            ctx.transform * Affine::translate((ix0 + 14.0, y + 38.0)),
            None,
        );
        let chip = Rect::new(ix1 - 64.0, y + 46.0, ix1 - 14.0, y + 54.0);
        ctx.fill_round(chip, 4.0, pal.accent);
        y + 72.0 + 22.0
    }

    /// The "Done" button that closes the editor.
    fn draw_scheme_editor_done_button(&mut self, ctx: &mut PaintCtx<'_>, ix1: f64, y: f64) {
        let theme = ctx.theme;
        let done_w = f64::from(
            ctx.text
                .measure_weighted("Done", 13.0, None, TextWeight::Semibold)
                .0,
        ) + 40.0;
        let drect = Rect::new(ix1 - done_w, y, ix1, y + 34.0);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(drect, "Done")
                .primary()
                .radius(17.0)
                .text_size(13.0)
                .weight(TextWeight::Semibold)
                .hovered(self.is_hover(drect)),
        );
        self.push_hit(drect, Action::CloseSchemeEditor);
    }

    fn draw_font_picker(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let r = self.rect;
        chrome::sheet_scrim(ctx.scene, ctx.transform, theme, r);
        self.push_hit(r, Action::CloseFontPicker);

        let pw = 380.0_f64.min(r.width() - 40.0);
        let ph = (r.height() - 120.0).clamp(200.0, 520.0);
        let px0 = r.x0 + (r.width() - pw) / 2.0;
        let py0 = r.y0 + (r.height() - ph) / 2.0;
        let panel = Rect::new(px0, py0, px0 + pw, py0 + ph);
        chrome::panel(ctx.scene, ctx.transform, theme, panel);
        self.push_hit(panel, Action::Swallow);
        ctx.text.draw_text_weighted(
            ctx.scene,
            "Add a system font",
            16.0,
            c.text_primary,
            ctx.transform * Affine::translate((px0 + 20.0, py0 + 16.0)),
            None,
            TextWeight::Bold,
        );
        let close = Rect::new(px0 + pw - 40.0, py0 + 14.0, px0 + pw - 16.0, py0 + 38.0);
        paint::close(
            ctx.scene,
            ctx.transform,
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
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &list,
        );
        let fams = ctx.text.system_families();
        let row_h = 40.0;
        let mut fy = list_top - self.picker_scroll;
        for fam in &fams {
            if fy > list.y1 || fy + row_h < list.y0 {
                fy += row_h;
                continue;
            }
            let rr = Rect::new(
                px0 + rhythm::SIBLING,
                fy,
                px0 + pw - rhythm::SIBLING,
                fy + row_h - rhythm::TIGHT,
            );
            chrome::row(
                ctx.scene,
                ctx.transform,
                theme,
                rr,
                f64::from(theme.tokens.radii.md),
                SelectionState::new(false, self.is_hover(rr), false),
                false,
            );
            // Preview each family by rendering its own name in it.
            ctx.text.draw_style(
                ctx.scene,
                fam,
                c.text_primary,
                ctx.transform * Affine::translate((px0 + 22.0, fy + 10.0)),
                None,
                &TextStyle::new(15.0).with_family(FontChoice::Named(fam.clone())),
            );
            self.push_hit(rr, Action::AddSystemFont(fam.clone()));
            fy += row_h;
        }
        ctx.scene.pop_layer();
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
    let ink = theme.on_surface(base).ink;
    (base, ink)
}

#[cfg(test)]
#[path = "app/interaction_tests.rs"]
mod interaction_tests;
