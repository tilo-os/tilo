//! The root widget: a toolbar, the image canvas, and a status line.
//!
//! # Composition
//!
//! Every control here is a shared toolkit widget. The buttons are
//! [`ui::widget::Button`], the canvas is [`ui::widget::ImageViewer`], and the
//! nothing-open state is [`ui::widget::EmptyState`]. Nothing is hand-rolled:
//! a bespoke button would have to re-earn focus, hover, disabled contrast,
//! accessibility roles and theming, and would drift from the rest of the
//! system the first time any of those changed.
//!
//! There is no title bar. The compositor owns window chrome; an app that drew
//! its own name at the top would be drawing it twice.
//!
//! # Where the pixels live
//!
//! The document holds a portable content reference and an [`Edits`] value.
//! This struct holds the decoded original and the rendered result, both
//! outside the CRDT, and re-derives the rendering whenever that reference or
//! the edits change. That check is what keeps a rotate from re-decoding the
//! file, and what keeps the decoded buffer from being rebuilt on every frame.

use std::cell::Cell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use std::sync::mpsc::{Receiver, SyncSender};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::ImageData;

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::icons::{Icon, draw_icon};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, EmptyState, ImageViewer, TextInput, Widget};
use winit::keyboard::{Key, NamedKey};

use crate::doc::PhotosDoc;
use crate::model::{Edits, LibraryPhoto, OpenImage, Rect01};
use crate::pixels::{self, Rgba};
use crate::strings::tr;

/// Toolbar height, and the status bar's. Both are chrome around the canvas,
/// which takes everything left over.
const BAR_H: f32 = 48.0;
const PAD: f32 = 12.0;

/// The photo-details sheet, authored at 1×: title 16 px down, subtitle at 48,
/// fields from 90 on a 60 px pitch (label, 38 px input), a 44 px footer whose
/// buttons end 20 px above the edge — 408 px in all. The spacing that used to
/// be `literal × text scale` still scales; the line boxes are now measured,
/// so a wrapped 200% subtitle pushes the fields down instead of under
/// "Folder", and the field list scrolls beneath the pinned footer when the
/// viewport caps the sheet (the footer used to paint over "Captured").
const SHEET_INSET: f64 = 16.0;
const SHEET_TOP_PAD: f64 = 16.0;
const SHEET_TITLE_GAP: f64 = 9.5;
const SHEET_SUBTITLE_GAP: f64 = 27.0;
const SHEET_LABEL_GAP: f64 = 2.0;
const SHEET_INPUT_H: f64 = 38.0;
const SHEET_FIELD_GAP: f64 = 5.0;
const SHEET_CONTENT_PAD_BOTTOM: f64 = 11.0;
const SHEET_BODY_FOOTER_GAP: f64 = 8.0;
const SHEET_FOOTER_H: f64 = 64.0;
const SHEET_BUTTON_H: f64 = 44.0;
const SHEET_TITLE: &str = "Photo details";
const SHEET_SUBTITLE: &str = "Shared labels only — no automatic people identification";
const SHEET_FIELD_LABELS: [&str; 4] = [
    "Folder",
    "People (comma-separated)",
    "Tags (comma-separated)",
    "Captured",
];

/// Button height. Deliberately shorter than the bar so the controls sit in
/// the strip rather than filling it edge to edge.
const BTN_H: f32 = 34.0;

/// The shared `Button` face paints its label at this renderer-relative size
/// (`Button::measure`), and the renderer applies the accessibility text scale
/// plus the default `TextStyle` line height. The toolbar capsules and the
/// status band keep their authored 1× heights until that measured line no
/// longer fits, then grow just enough to contain it — so 200% labels stop
/// colliding with the capsule edge while 1× density is untouched.
/// (Alignment review 2026-09-16; kept local to this file because the toolbar
/// is sized in `build_layout`, which sees the theme but no renderer.)
const BUTTON_LABEL_SIZE: f32 = 14.0;
const STATUS_TEXT_SIZE: f32 = 13.0;
const BTN_PAD_Y: f32 = 8.0;
const STATUS_PAD_Y: f32 = 4.0;

/// Line height the renderer will give `size` at `text_scale` — the default
/// `TextStyle` leading on the scaled size, not a local copy of that factor.
fn line_h(size: f32, text_scale: f32) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a line height in logical px fits f32 exactly as the renderer reports it"
    )]
    let line = ui::text_metrics::line_height_px(size, text_scale) as f32;
    line
}

/// Toolbar capsule height at `text_scale`: the authored [`BTN_H`] at 1×.
fn btn_h(text_scale: f32) -> f32 {
    BTN_H.max(line_h(BUTTON_LABEL_SIZE, text_scale) + 2.0 * BTN_PAD_Y)
}

/// Status band height at `text_scale`: the authored `BAR_H * 0.6` at 1×, and
/// tall enough to contain the filename line at any scale (plan task 19).
fn status_h(text_scale: f32) -> f32 {
    (BAR_H * 0.6).max(line_h(STATUS_TEXT_SIZE, text_scale) + 2.0 * STATUS_PAD_Y)
}

/// A library card's action row: Details on the left, remove and favourite on
/// the right, all inset [`CARD_ACTION_EDGE_INSET`] from the card's edges.
///
/// This is the *one* description of that row. `draw_library` paints these
/// rects, `handle_event` hit-tests them, `accessibility_tree` exports them and
/// `library_card_action_rects` hands them to tests — four call sites that used
/// to carry their own copies of the arithmetic (and the right-hand cluster ran
/// flush to `card.x1`, 5 px from the edge against Details' 12).
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardActions {
    details: Rect,
    remove: Rect,
    favorite: Rect,
}

const CARD_ACTION_EDGE_INSET: f64 = 8.0;
const CARD_ACTION_ROW_H: f64 = 44.0;
const CARD_ACTION_CONTROL_W: f64 = 44.0;

fn card_action_controls(card: Rect) -> CardActions {
    let action_y = card.y1 - CARD_ACTION_ROW_H;
    let favorite = Rect::new(
        card.x1 - CARD_ACTION_EDGE_INSET - CARD_ACTION_CONTROL_W,
        action_y,
        card.x1 - CARD_ACTION_EDGE_INSET,
        card.y1,
    );
    let remove = Rect::new(
        favorite.x0 - CARD_ACTION_CONTROL_W,
        action_y,
        favorite.x0,
        card.y1,
    );
    let details = Rect::new(
        card.x0 + CARD_ACTION_EDGE_INSET,
        action_y,
        (card.x0 + 108.0).min(remove.x0),
        card.y1,
    );
    CardActions {
        details,
        remove,
        favorite,
    }
}

/// Per-character label width used to size a button in `build_layout`, which
/// gets a [`Theme`] but no [`TextRenderer`] to shape text with.
///
/// An **upper bound**, not an average. The shared [`ui::widget::Toolbar`]
/// estimates 7.0 for the same job, which is too tight for this one: at 14 px
/// Medium, "Open" shapes to 36.5 px — 9.13 a character — so a 7.0 budget left
/// the label 8.5 px wider than the button drawn around it, and `Button::paint`
/// centres the cluster, so it spilled out of both ends onto its neighbours.
/// `toolbar_labels_fit_inside_the_buttons_they_are_sized_for` in
/// `tests/end_to_end.rs` shapes the real glyphs and holds this bound.
pub const APPROX_CHAR_W: f32 = 10.0;

/// Glyph box, and the gap to a label beside it. `Button::paint` derives the
/// icon from the button height as `(h - 14).clamp(12, 22)`, which at
/// [`BTN_H`] is exactly this.
const ICON_W: f32 = BTN_H - 14.0;
const ICON_GAP: f32 = 6.0;
/// Padding either side of an icon-and-label cluster.
const LABEL_PAD_X: f32 = 12.0;
/// Gap between adjacent buttons.
const BTN_GAP: f32 = 6.0;
/// Horizontal room a group separator occupies, rule included.
const SEP_W: f32 = 17.0;

/// Breathing room between the photograph and the chrome above and below it.
/// Without it a contain-fit image whose limiting axis is the height butts
/// straight against the toolbar rule and looks wedged under it.
const CANVAS_INSET: f32 = 10.0;

/// Library previews are recognition aids, never an alternate full-resolution
/// image cache. One small worker and a four-item queue keep a hostile library
/// from occupying a core or retaining unbounded decoded pixels.
const THUMBNAIL_MAX_W: u32 = 320;
const THUMBNAIL_MAX_H: u32 = 180;
const THUMBNAIL_QUEUE_LIMIT: usize = 4;
/// One bounded materialized library page. This is intentionally independent
/// of the number of cards that happen to fit in a particular window.
const LIBRARY_PAGE_SIZE: usize = 24;

/// A four-column shelf is only useful while each photograph remains a
/// recognisable, comfortably targetable card. At tablet widths a two-column
/// shelf works well; below this a single card preserves independent Details,
/// favourite, and removal targets at the 44px touch minimum.
const LIBRARY_TWO_COLUMN_MAX_W: f64 = 720.0;

/// Grid density is presentation-local, but text scale is part of that
/// presentation. Keep the decision pure so wide/high-text and sheet-open
/// regressions cannot silently drift apart.
fn library_column_count(width: f64, large_text: bool, metadata_editor: bool) -> usize {
    crate::presentation::library_column_count(width, large_text, metadata_editor)
}

#[derive(Clone)]
struct ThumbnailJob {
    generation: u64,
    id: String,
    image: OpenImage,
}

struct ThumbnailResult {
    generation: u64,
    id: String,
    preview: Option<Rgba>,
}

fn thumbnail_worker(rx: &Receiver<ThumbnailJob>, tx: &SyncSender<ThumbnailResult>) {
    while let Ok(job) = rx.recv() {
        let preview = crate::doc::bytes_of(&job.image)
            .and_then(|bytes| pixels::decode(&bytes))
            .map(|image| pixels::thumbnail(&image, THUMBNAIL_MAX_W, THUMBNAIL_MAX_H));
        if tx
            .send(ThumbnailResult {
                generation: job.generation,
                id: job.id,
                preview,
            })
            .is_err()
        {
            return;
        }
    }
}

/// One toolbar affordance. Buttons post their intent here rather than
/// mutating the document from inside a closure, because the document is not
/// reachable from a `FnMut()` — it arrives in `handle_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Open,
    Library,
    RotateLeft,
    RotateRight,
    CropToView,
    Reset,
    Copy,
    Share,
    Save,
    More,
}

/// Order is stable for both the compact popup's keyboard traversal and its
/// semantic menu. The commands themselves remain the normal toolbar actions.
const COMPACT_MORE_ENTRIES: [Hit; 5] = [
    Hit::CropToView,
    Hit::Reset,
    Hit::Copy,
    Hit::Share,
    Hit::Save,
];

/// Controls inside the in-grid album editor. They are intentionally a small
/// local presentation state machine; every durable outcome is still one
/// `PhotosAction::SaveAlbum` action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlbumEditorHit {
    New,
    Edit,
    PreviousPage,
    NextPage,
    Save,
    Cancel,
    Delete,
    ConfirmDelete,
}

/// Controls in the photo-details sheet.  The fields themselves are shared
/// `TextInput`s; these are only the surrounding sheet actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataEditorHit {
    Save,
    Cancel,
}

/// A compact materialised card.  The full library remains canonical in the
/// document; this carries just enough metadata to make the visible shelf
/// scannable without another document walk while painting.
#[derive(Clone)]
struct LibraryItem {
    id: String,
    thumbnail_key: String,
    name: String,
    favorite: bool,
    metadata: String,
}

/// How a toolbar item presents itself.
///
/// An [`Face::IconOnly`] button is not a mute one: the label rides along as
/// the tooltip, which [`ui::widget::Button`] also adopts as the accessible
/// name, so the glyph stays reachable by pointer-dwell and by screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Glyph and words together.
    Labelled,
    /// A deliberately wide labelled overflow trigger used only for the
    /// large-text compact toolbar.  It keeps the word “More” visible instead
    /// of asking a pointer user to infer a horizontal-ellipsis glyph.
    LargeOverflow,
    /// Glyph alone.
    IconOnly,
}

/// What the canvas can honestly render right now. A shared document can name
/// an image before this device has received its content, so "no pixels yet"
/// is distinct from both an empty library and corrupt local bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CanvasState {
    #[default]
    Empty,
    Loading,
    Error,
    Ready,
}

/// A slot in the toolbar strip, in painting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// Index into [`PhotosApp::buttons`].
    Item(usize),
    /// A hairline dividing two groups.
    Separator,
    /// Greedy gap: soaks the leftover width so what follows sits hard right.
    Spacer,
}

/// The toolbar, in order: what opens a file, what adjusts it, what gets the
/// result out.
///
/// The four adjusters are glyph-only at every width. They are the operations
/// whose icons are unambiguous, and spelling all seven out is what pushed the
/// strip to a fixed 912 px — wider than a half-tiled window, which silently
/// clipped Save off the right-hand edge.
///
/// Three of the seven glyphs below stand in for icons the shared set does not
/// have yet (`RotateCcw`, a crop mark, a disk): `RotateLeft` reuses
/// [`Icon::RotateCw`] (the same glyph as its neighbour, told apart by tooltip
/// and accessible name — a real but bounded gap), `CropToView` reuses
/// [`Icon::Scissors`], and `Save` reuses [`Icon::Download`]. Adding the exact
/// glyphs is `ui::icons` work, out of this app's scope.
const TOOLBAR: &[(Hit, &str, Icon, Face)] = &[
    (Hit::Open, "photos-open", Icon::Folder, Face::Labelled),
    (Hit::Library, "photos-library", Icon::Image, Face::Labelled),
    (
        Hit::RotateLeft,
        "photos-rotate-left",
        Icon::RotateCw,
        Face::IconOnly,
    ),
    (
        Hit::RotateRight,
        "photos-rotate-right",
        Icon::RotateCw,
        Face::IconOnly,
    ),
    (
        Hit::CropToView,
        "photos-crop",
        Icon::Scissors,
        Face::IconOnly,
    ),
    (Hit::Reset, "photos-reset", Icon::Refresh, Face::IconOnly),
    (Hit::Copy, "photos-copy", Icon::Copy, Face::Labelled),
    (Hit::Share, "photos-share", Icon::Send, Face::Labelled),
    (Hit::Save, "photos-save", Icon::Download, Face::Labelled),
    (
        Hit::More,
        "photos-more",
        Icon::MoreHorizontal,
        Face::Labelled,
    ),
];

/// The strip's shape: Open, a rule, the adjusters, then everything that gets
/// the photograph out, pushed to the trailing edge.
const SLOTS: &[Slot] = &[
    Slot::Item(0),
    Slot::Item(1),
    Slot::Separator,
    Slot::Item(2),
    Slot::Item(3),
    Slot::Item(4),
    Slot::Item(5),
    Slot::Spacer,
    Slot::Item(6),
    Slot::Item(7),
    Slot::Item(8),
];

/// Phone chrome keeps the two navigation verbs and the two fast rotations
/// visible; less-frequent destructive/export operations live in an explicit
/// labeled More menu rather than becoming an indistinguishable icon run.
const COMPACT_SLOTS: &[Slot] = &[
    Slot::Item(0),
    Slot::Item(1),
    // At phone width the labelled Library and More controls already create
    // the useful groups; dropping the extra rule buys a real 24 px safety
    // margin, so neither edge control is clipped at 390 px.
    Slot::Item(2),
    Slot::Item(3),
    Slot::Spacer,
    Slot::Item(9),
];

/// Width a button needs to show `face` with `label`.
/// Width of one toolbar item whose capsule is `btn_h` tall ([`btn_h`] at the
/// current text scale; [`BTN_H`] at 1×). An icon-only item is square.
fn item_width(face: Face, label: &str, btn_h: f32) -> f32 {
    match face {
        Face::IconOnly => btn_h,
        Face::Labelled => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(btn_h)
        }
        // The normal estimate is calibrated for 100% text.  At the supported
        // 200% screenshot scale the overflow label needs a real word-sized
        // slot, while every other command is represented by an accessible
        // icon in the compact menu.
        Face::LargeOverflow => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W * 2.0;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(btn_h)
        }
    }
}

/// Width the whole strip needs, `faces` giving each item's presentation.
///
/// Exposed so a test can assert the strip fits a window rather than
/// discovering it does not by finding Save missing from a screenshot.
pub fn toolbar_width(faces: &[Face]) -> f32 {
    toolbar_width_at(faces, BTN_H)
}

/// [`toolbar_width`] for capsules `btn_h` tall — the height `refit_toolbar`
/// builds the buttons with, so the fit decision uses the same widths the strip
/// will actually lay out at the theme's text scale.
fn toolbar_width_at(faces: &[Face], btn_h: f32) -> f32 {
    let mut w = PAD * 2.0;
    for (i, slot) in SLOTS.iter().enumerate() {
        if i > 0 {
            w += BTN_GAP;
        }
        w += match *slot {
            // Unreachable: `faces` and `TOOLBAR` are always built the same
            // length, one entry per `SLOTS::Item` index.
            Slot::Item(n) => match (faces.get(n), TOOLBAR.get(n)) {
                (Some(&face), Some(item)) => item_width(face, &tr(item.1), btn_h),
                _ => 0.0,
            },
            Slot::Separator => SEP_W,
            Slot::Spacer => 0.0,
        };
    }
    w
}

/// Each item's presentation at a given strip width, for capsules `btn_h` tall.
///
/// The labelled items give up their words rather than the strip giving up an
/// item: a control that has shrunk to its glyph is still reachable, one that
/// has fallen off the edge is not.
fn faces_for_width(width: f32, btn_h: f32) -> Vec<Face> {
    let roomy: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
    // Width 0 is the first frame, before a layout has happened. Assume room;
    // the next frame corrects it with a real measurement.
    if width <= 0.0 || toolbar_width_at(&roomy, btn_h) <= width {
        return roomy;
    }
    if width < 600.0 {
        return TOOLBAR
            .iter()
            .enumerate()
            .map(|(index, _)| {
                // Open keeps its tooltip and accessible name when compact;
                // spending that width on the explicit More label is the
                // clearer trade on a 390 px phone.
                if index == 9 || (width >= 380.0 && index == 1) {
                    Face::Labelled
                } else {
                    Face::IconOnly
                }
            })
            .collect();
    }
    TOOLBAR.iter().map(|_| Face::IconOnly).collect()
}

/// Authored content observed by local transient work, excluding operational metadata.
#[derive(Clone, PartialEq)]
struct LiveContent {
    image: Option<OpenImage>,
    edits: Edits,
    library: crate::model::PhotoLibrary,
}
impl LiveContent {
    fn read(doc: &AutoCommit) -> Self {
        Self {
            image: PhotosDoc::image(doc),
            edits: PhotosDoc::edits(doc),
            library: PhotosDoc::library(doc),
        }
    }
}

/// The rendered image, and the inputs it was rendered from.
///
/// Keeping the inputs beside the output is what lets `sync` decide in a couple
/// of comparisons whether anything needs redoing.
struct Rendered {
    identity: String,
    edits: Edits,
    /// The edited pixels, kept so Copy and Save export exactly what is shown
    /// rather than re-running the pipeline and hoping it agrees.
    rgba: Rgba,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "library mode and the three independent transient album-editor flags have distinct event and reconciliation lifecycles"
)]
pub struct PhotosApp {
    /// Canonical dispatcher shared by toolbar edits and exported document actions.
    actions: ui::ActionHost,
    /// Decoded original, before any edit. `None` until a file opens.
    original: Option<(String, Rgba)>,
    original_identity: Option<String>,
    observed_content: Option<LiveContent>,
    import_content: Option<LiveContent>,
    picker_stale: bool,
    rendered: Option<Rendered>,
    viewer: Option<ImageViewer>,
    buttons: Vec<(Hit, Button)>,
    pending: Rc<Cell<Option<Hit>>>,
    /// Where the last export went, or why it failed. Shown in the status line
    /// so an action is never silent.
    notice: Option<String>,
    /// The status line, recomputed in `sync` — `draw` has no document.
    status: String,
    rect: Rect,
    toolbar_rect: Rect,
    canvas_rect: Rect,
    status_rect: Rect,
    /// Where each group rule landed, filled in by `apply_layout` so `draw`
    /// paints them without re-deriving the strip's geometry.
    separator_rects: Vec<Rect>,
    /// Presentation of each toolbar item at the width last laid out.
    faces: Vec<Face>,
    /// Kept local: text preference changes presentation, never document
    /// contents or another collaborator's toolbar.
    large_text_toolbar: bool,
    /// Capsule height the toolbar was last built for ([`btn_h`] at the
    /// theme's text scale). A change rebuilds the strip like a face change.
    toolbar_btn_h: f32,
    compact_more_open: bool,
    compact_more_focus: usize,
    compact_more_hits: Vec<(Hit, Rect)>,
    /// Portal channel while a file chooser is open.
    picker: Option<ui::portal::PortalChannel>,
    resource_invocation: Option<qos_actions::InvocationId>,
    resource_result: Option<qos_actions::InvocationUpdate>,
    pending_resource_open: Option<(qos_actions::InvocationId, String)>,
    canvas_state: CanvasState,
    search_provider: crate::search::PhotosSearchProvider,
    /// Heads that produced the cached immutable/Arc-backed search catalogue.
    /// An unchanged `sync` must not rebuild a full-library index.
    search_heads: Vec<automerge::ChangeHash>,
    /// The library is deliberately a bounded presentation slice.  The CRDT
    /// can hold considerably more entries than a window can paint or an
    /// assistive technology should receive in one update.
    library_items: Vec<LibraryItem>,
    library_total: usize,
    library_offset: usize,
    library_albums: Vec<(String, String, usize)>,
    active_album: Option<String>,
    /// Metadata browse filters originate from canonical folder/person
    /// resources. They are local presentation state, just like an album view;
    /// changing a filter must never rewrite another collaborator's metadata.
    active_folder: Option<String>,
    active_person: Option<String>,
    /// Tag filtering is local navigation derived from a durable tag resource.
    /// It never mutates a collaborator's metadata just by opening a view.
    active_tag: Option<String>,
    /// Bounded labels used for discoverable visual filtering. Search and the
    /// resource tree still resolve every stable id outside this shelf.
    library_folders: Vec<(String, usize)>,
    library_people: Vec<(String, usize)>,
    metadata_filter_cards: Vec<(Option<String>, Option<String>, Rect)>,
    library_details_cards: Vec<(String, Rect)>,
    album_cards: Vec<(Option<String>, Rect)>,
    album_editor: bool,
    album_editor_id: Option<String>,
    album_editor_creating: bool,
    album_name_input: TextInput,
    album_editor_members: HashSet<String>,
    album_delete_confirm: bool,
    /// Tab order inside the transient sheet: name, cancel, delete (when
    /// editing), save. The `TextInput` remains the actual focused widget.
    album_editor_focus: usize,
    album_editor_rect: Rect,
    album_editor_hits: Vec<(AlbumEditorHit, Rect)>,
    /// Shared, retained controls for moving through the local library shelf
    /// while selecting album membership. They never write the document.
    album_page_buttons: Vec<(AlbumEditorHit, Button)>,
    album_page_pending: Rc<Cell<Option<AlbumEditorHit>>>,
    metadata_editor: bool,
    metadata_editor_id: Option<String>,
    metadata_folder_input: TextInput,
    metadata_people_input: TextInput,
    metadata_tags_input: TextInput,
    metadata_captured_at_input: TextInput,
    /// Tab order in the details sheet: four shared fields, Cancel, Save.
    metadata_editor_focus: usize,
    metadata_editor_hits: Vec<(MetadataEditorHit, Rect)>,
    /// Scroll offset of the sheet's field list. The sheet is capped at the
    /// viewport and its footer is pinned, so when the fields need more room
    /// than that leaves (200% text on an 800×900 window) they scroll under
    /// the footer instead of the footer painting over the last field.
    metadata_editor_scroll: f64,
    /// The field list's visible region from the last paint — where wheel
    /// events scroll and outside which presses are not forwarded to a
    /// scrolled-away input.
    metadata_editor_body: Rect,
    /// Set when keyboard focus moves; the next paint scrolls that field into
    /// view.
    metadata_editor_reveal_focus: bool,
    /// At most two viewportfuls of decoded thumbnails.  Originals are never
    /// scanned at draw time, and a large library cannot turn a frame into a
    /// disk-I/O loop.
    library_thumbnails: HashMap<String, ImageData>,
    /// The visible entries whose bytes were unavailable or invalid in the
    /// current document generation. Keeping this negative cache avoids
    /// decoding the same corrupt peer payload every frame; it is cleared when
    /// shared metadata changes so a fresh content reference gets a retry.
    thumbnail_unavailable: HashSet<String>,
    /// A bad peer-provided image must not be decoded once per frame. Content
    /// hashes are immutable, and a user reopening a changed legacy path makes
    /// a new attempt through `open_path`, so retaining this failure is safe.
    original_decode_failed: Option<String>,
    /// One background decoder serializes disk/content-store access. Results
    /// are stamped with a visible-library generation before they may enter the
    /// UI cache, so an old job can never paint a card that now names another
    /// photograph.
    thumbnail_tx: Option<SyncSender<ThumbnailJob>>,
    thumbnail_rx: Receiver<ThumbnailResult>,
    /// `None` is a deliberately terminal generation. Counter exhaustion is
    /// practically unreachable, but reusing zero could admit an ancient
    /// queued result, so the cache fails closed instead.
    thumbnail_generation: Option<u64>,
    thumbnail_visible_ids: Vec<String>,
    thumbnail_queued: HashSet<String>,
    /// A destructive library removal takes two deliberate activations. The
    /// stable photo id, not a card index, keeps confirmation correct if a
    /// collaborator changes the grid between clicks.
    library_remove_confirm: Option<String>,
    /// Stable-id hit targets for the currently materialised grid cards.
    library_cards: Vec<(String, Rect, bool)>,
    pointer: Point,
    library_mode: bool,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    fn toolbar_slots(&self) -> &'static [Slot] {
        if self.large_text_toolbar || (self.rect.width() > 0.0 && self.rect.width() < 600.0) {
            COMPACT_SLOTS
        } else {
            SLOTS
        }
    }

    pub fn new() -> Self {
        let (thumbnail_job_tx, thumbnail_job_rx) =
            std::sync::mpsc::sync_channel(THUMBNAIL_QUEUE_LIMIT);
        let (thumbnail_result_tx, thumbnail_result_rx) =
            std::sync::mpsc::sync_channel(THUMBNAIL_QUEUE_LIMIT);
        let thumbnail_tx = std::thread::Builder::new()
            .name("photos-thumbnail".to_owned())
            .spawn(move || thumbnail_worker(&thumbnail_job_rx, &thumbnail_result_tx))
            .ok()
            .map(|_| thumbnail_job_tx);
        let pending: Rc<Cell<Option<Hit>>> = Rc::new(Cell::new(None));
        let album_page_pending: Rc<Cell<Option<AlbumEditorHit>>> = Rc::new(Cell::new(None));
        let faces: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
        let mut buttons = Vec::new();
        for ((hit, key, icon, _), face) in TOOLBAR.iter().zip(faces.iter()) {
            // 1× capsules; the first `build_layout` refits them to the theme.
            buttons.push((*hit, build_button(*hit, key, *icon, *face, BTN_H, &pending)));
        }
        Self {
            actions: ui::ActionHost::new(),
            original: None,
            original_identity: None,
            observed_content: None,
            import_content: None,
            picker_stale: false,
            rendered: None,
            viewer: None,
            buttons,
            pending,
            notice: None,
            status: String::new(),
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            separator_rects: Vec::new(),
            faces,
            large_text_toolbar: false,
            toolbar_btn_h: BTN_H,
            compact_more_open: false,
            compact_more_focus: 0,
            compact_more_hits: Vec::new(),
            picker: None,
            resource_invocation: None,
            resource_result: None,
            pending_resource_open: None,
            canvas_state: CanvasState::Empty,
            search_provider: crate::search::PhotosSearchProvider::empty(),
            search_heads: Vec::new(),
            library_items: Vec::new(),
            library_total: 0,
            library_offset: 0,
            library_albums: Vec::new(),
            active_album: None,
            active_folder: None,
            active_person: None,
            active_tag: None,
            library_folders: Vec::new(),
            library_people: Vec::new(),
            metadata_filter_cards: Vec::new(),
            library_details_cards: Vec::new(),
            album_cards: Vec::new(),
            album_editor: false,
            album_editor_id: None,
            album_editor_creating: false,
            album_name_input: TextInput::local().with_id("photos-album-name"),
            album_editor_members: HashSet::new(),
            album_delete_confirm: false,
            album_editor_focus: 0,
            album_editor_rect: Rect::ZERO,
            album_editor_hits: Vec::new(),
            album_page_buttons: build_album_page_buttons(&album_page_pending),
            album_page_pending,
            metadata_editor: false,
            metadata_editor_id: None,
            metadata_folder_input: TextInput::local().with_id("photos-metadata-folder"),
            metadata_people_input: TextInput::local().with_id("photos-metadata-people"),
            metadata_tags_input: TextInput::local().with_id("photos-metadata-tags"),
            metadata_captured_at_input: TextInput::local().with_id("photos-metadata-captured-at"),
            metadata_editor_focus: 0,
            metadata_editor_hits: Vec::new(),
            metadata_editor_scroll: 0.0,
            metadata_editor_body: Rect::ZERO,
            metadata_editor_reveal_focus: false,
            library_thumbnails: HashMap::new(),
            thumbnail_unavailable: HashSet::new(),
            original_decode_failed: None,
            thumbnail_tx,
            thumbnail_rx: thumbnail_result_rx,
            thumbnail_generation: Some(0),
            thumbnail_visible_ids: Vec::new(),
            thumbnail_queued: HashSet::new(),
            library_remove_confirm: None,
            library_cards: Vec::new(),
            pointer: Point::ZERO,
            library_mode: false,
        }
    }

    /// Rebuild the strip for `width` if the room available has changed which
    /// items can afford their labels.
    ///
    /// Rebuilding rather than relabelling in place because a `Button` owns its
    /// label for its lifetime. The cost lands only on a resize that crosses
    /// the threshold, and it carries the enabled state across so a rebuild
    /// never re-enables a control the document says is unavailable.
    fn refit_toolbar(&mut self, width: f32, btn_h: f32) {
        let want = if self.large_text_toolbar {
            TOOLBAR
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    if index == 9 {
                        Face::LargeOverflow
                    } else {
                        Face::IconOnly
                    }
                })
                .collect()
        } else {
            faces_for_width(width, btn_h)
        };
        // Both heights come from the same pure `btn_h()` on the theme's scale,
        // so exact comparison is the honest one.
        #[allow(
            clippy::float_cmp,
            reason = "outputs of one pure function on one input"
        )]
        if want == self.faces && btn_h == self.toolbar_btn_h {
            return;
        }
        let enabled: Vec<bool> = self.buttons.iter().map(|(_, b)| b.is_enabled()).collect();
        self.buttons.clear();
        for (((hit, key, icon, _), face), was_enabled) in
            TOOLBAR.iter().zip(want.iter()).zip(enabled)
        {
            let mut button = build_button(*hit, key, *icon, *face, btn_h, &self.pending);
            button.set_enabled(was_enabled);
            self.buttons.push((*hit, button));
        }
        self.faces = want;
        self.toolbar_btn_h = btn_h;
    }

    /// Import `path` through the canonical durable action. The decoded source
    /// path remains only in this process; the action writes portable metadata
    /// and the staged content hash into the shared document.
    pub fn open_path(&mut self, doc: &mut AutoCommit, path: &str) {
        let bytes = std::fs::read(path).ok();
        match bytes.as_deref().and_then(pixels::decode) {
            Some(img) => {
                // Put the bytes in the content store before naming them, so
                // the hash this document is about to carry is one this machine
                // can already answer for — and one the room can be handed.
                // Otherwise the document would name a picture only the path
                // could find, and the path is true on one machine.
                let Some(content) = qdoc::blobstore::put(bytes.as_deref().unwrap_or_default())
                else {
                    self.notice = Some(tr("photos-open-failed"));
                    return;
                };
                let open = OpenImage::from_path(path, img.w, img.h).with_content(content);
                self.original = Some((path.to_string(), img));
                self.original_identity = Some(decode_key(&open.clone().portable()));
                self.rendered = None;
                self.viewer = None;
                self.original_decode_failed = None;
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::ImportPhoto {
                        photo: LibraryPhoto::from_image(open),
                    },
                );
                self.notice = None;
            }
            None => {
                self.notice = Some(tr("photos-open-failed"));
            }
        }
    }

    /// The path of the photograph this instance is showing, if any.
    ///
    /// Read off the decoded original rather than the document, because the two
    /// answer different questions and only this one is true of the *pixels on
    /// screen*: `refresh` reloads it from the document and clears it when the
    /// file will not decode, so a path here means there is something drawn to
    /// point at. A document naming a file this machine cannot read would
    /// otherwise publish a rect around an empty canvas.
    pub fn open_image_path(&self) -> Option<&str> {
        self.original.as_ref().map(|(p, _)| p.as_str())
    }

    /// The pixels currently shown, if any. This is what Copy and Save export.
    pub fn edited_pixels(&self) -> Option<&Rgba> {
        self.rendered.as_ref().map(|r| &r.rgba)
    }

    /// The rect the canvas was laid out into. The photograph is drawn inside
    /// this and nowhere else.
    pub fn canvas_rect(&self) -> Rect {
        self.canvas_rect
    }

    /// The toolbar's rect.
    pub fn toolbar_rect(&self) -> Rect {
        self.toolbar_rect
    }

    /// The status line's rect.
    pub fn status_rect(&self) -> Rect {
        self.status_rect
    }

    /// Every toolbar control as `(accessible name, rect)`, in strip order.
    ///
    /// Exposed so a test can assert that each control is actually within the
    /// window. `canvas_rect` staying inside the frame says nothing about the
    /// strip above it, which is how a fixed-width toolbar came to push Save
    /// off the right-hand edge of any window narrower than 912 px while every
    /// geometry assertion still passed.
    pub fn toolbar_item_rects(&self) -> Vec<(String, Rect)> {
        // Unreachable: `self.buttons` is always built one entry per `TOOLBAR`
        // item, so every index here has a matching `TOOLBAR` entry.
        self.buttons
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                self.toolbar_slots()
                    .iter()
                    .any(|slot| matches!(slot, Slot::Item(item) if *item == *index))
            })
            .filter_map(|(i, (_, b))| TOOLBAR.get(i).map(|item| (tr(item.1), b.get_rect())))
            .collect()
    }

    /// Measured library-card geometry for screenshot and interaction
    /// regressions. Every returned tuple is `(card, details, remove,
    /// favorite)`; the three controls are their actual 44px hit rectangles,
    /// not the smaller icon artwork inset inside them.
    pub fn library_card_action_rects(&self) -> Vec<(Rect, Rect, Rect, Rect)> {
        self.library_cards
            .iter()
            .filter_map(|(id, card, _)| {
                let details = self
                    .library_details_cards
                    .iter()
                    .find(|(candidate, _)| candidate == id)
                    .map(|(_, bounds)| *bounds)?;
                let actions = card_action_controls(*card);
                Some((*card, details, actions.remove, actions.favorite))
            })
            .collect()
    }

    /// The status line as it currently reads.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Whether a photograph is loaded and rendered.
    pub fn has_image(&self) -> bool {
        self.viewer.is_some()
    }

    /// Reveal the bounded library grid.  Kept public for the deterministic
    /// offscreen gallery; ordinary interaction reaches it through the same
    /// toolbar button and semantic action as a user.
    pub fn show_library(&mut self) {
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.library_remove_confirm = None;
    }

    /// Pose the ordinary library editor for deterministic offscreen coverage.
    /// The production entry point is the visible “Edit album” control; shot
    /// rendering has no window event stream to click it through.
    pub fn show_album_editor_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        self.active_album = PhotosDoc::library(doc)
            .albums
            .first()
            .map(|album| album.id.clone());
        self.begin_edit_album(doc);
    }

    /// Pose the shared metadata editor for visual regression coverage.
    pub fn show_metadata_editor_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        let library = PhotosDoc::library(doc);
        if let Some(id) = library
            .photos
            .iter()
            .find(|photo| !photo.metadata_summary().is_empty())
            .or_else(|| library.photos.first())
            .map(|photo| photo.id.clone())
        {
            self.begin_metadata_editor(doc, &id);
        }
    }

    /// Zoom the canvas, as the scroll wheel would. Lets a caller frame a crop
    /// without simulating a chain of scroll events by hand.
    ///
    /// `ImageViewer` exposes no direct zoom setter to a widget that only
    /// depends on it (only a `#[cfg(test)]` one, private to that crate), so
    /// this drives the same `MouseWheel` path a real scroll takes: park the
    /// cursor at the viewer's own centre — where the widget's screen-to-image
    /// mapping is invariant under a zoom change, so pan stays exactly where it
    /// was — then send one wheel notch sized to land on `zoom` exactly.
    pub fn set_zoom(&mut self, zoom: f32) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        let rect = Widget::get_rect(v);
        if rect.width() <= 0.0 || rect.height() <= 0.0 || v.zoom() <= 0.0 {
            return;
        }
        let center =
            winit::dpi::PhysicalPosition::new((rect.x0 + rect.x1) * 0.5, (rect.y0 + rect.y1) * 0.5);
        // Solves `v.zoom() * ZOOM_STEP.powf(scroll_y) == zoom` for the notch
        // the widget's own wheel handler would need to see. `1.1` mirrors the
        // private `ZOOM_STEP` constant in `ui::widget::image_viewer` — there is
        // no public way to ask the widget for it.
        let scroll_y = (zoom / v.zoom()).ln() / 1.1_f32.ln();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        // A fresh leaf on a fresh tree cannot fail; bail out rather than
        // panic if it somehow does.
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        // SAFETY: `DeviceId` has no public constructor; a zeroed value is
        // winit's own "no device" sentinel, and every field below is
        // overwritten or unused by `ImageViewer::handle_event`. Same idiom
        // `ui::widget::image_viewer`'s own tests use.
        let device_id = unsafe { std::mem::zeroed() };
        v.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id,
                position: center,
            },
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        v.handle_event(
            &winit::event::WindowEvent::MouseWheel {
                device_id,
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, scroll_y),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// The visible region of the canvas in normalized image coordinates, or
    /// the whole image when the viewer has not been laid out yet.
    ///
    /// `ImageViewer` exposes no direct query for this either, so it is derived
    /// in [`visible_image_rect`] from the same "contain fit, then zoom and
    /// pan" transform `ImageViewer::draw` applies — using only the widget's
    /// already-public `zoom()` / `pan()` / rect, and this app's own knowledge
    /// of the original image's pixel size.
    pub fn visible_rect(&self) -> Rect01 {
        match (self.viewer.as_ref(), self.original.as_ref()) {
            (Some(v), Some((_, original))) => {
                visible_image_rect(v, f64::from(original.w), f64::from(original.h))
                    .unwrap_or(Rect01::FULL)
            }
            _ => Rect01::FULL,
        }
    }

    /// Copy what is shown to the system clipboard as raw RGBA.
    ///
    /// Raw rather than PNG because that is what the clipboard carries between
    /// sandboxes here — [`ui::clipboard_copy_image`] hands the buffer to the
    /// compositor's selection, and the receiving app gets pixels without
    /// either side agreeing on a container format.
    pub fn copy_to_clipboard(&mut self) -> bool {
        let Some(img) = self.edited_pixels() else {
            return false;
        };
        let ok = ui::clipboard_copy_image(img.w as usize, img.h as usize, &img.bytes);
        self.notice = Some(if ok {
            tr("photos-copied")
        } else {
            tr("photos-copy-failed")
        });
        ok
    }

    /// Write what is shown to `path` as a PNG.
    pub fn save_to(&mut self, path: &str) -> bool {
        let Some(bytes) = self.edited_pixels().and_then(pixels::encode_png) else {
            self.notice = Some(tr("photos-save-failed"));
            return false;
        };
        if let Ok(()) = std::fs::write(path, &bytes) {
            self.notice = Some(format!("{} {path}", tr("photos-saved")));
            true
        } else {
            self.notice = Some(tr("photos-save-failed"));
            false
        }
    }

    /// Default save destination: the source file with `-edited` before the
    /// extension, so Save never silently overwrites the original.
    pub fn suggested_save_path(open: &OpenImage) -> String {
        let p = std::path::Path::new(&open.path);
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = p
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        parent
            .join(format!("{stem}-edited.png"))
            .to_string_lossy()
            .into_owned()
    }

    /// Apply `next` to the document as an undoable edit.
    fn commit_edits(&mut self, doc: &mut AutoCommit, next: Edits) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            &crate::actions::PhotosAction::set(&next),
            qos_actions::ActionContext::for_action(
                "org.qos.Photos",
                "Photos",
                &crate::actions::PhotosAction::set(&next),
            ),
        );
        self.notice = None;
    }

    /// Every card interaction routes through the same typed, replayable
    /// action registry as resources and search results.  The canvas never
    /// writes document fields directly.
    fn commit_library_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::PhotosAction,
    ) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Photos", "Photos", &action),
        );
    }

    fn begin_new_album(&mut self, doc: &AutoCommit) {
        let library = PhotosDoc::library(doc);
        let mut suffix = 1_u32;
        let id = loop {
            let candidate = format!("photos.album:untitled-{suffix}");
            if library.album(&candidate).is_none() {
                break candidate;
            }
            suffix += 1;
        };
        self.album_editor = true;
        self.album_editor_creating = true;
        self.album_editor_id = Some(id);
        self.album_name_input.set_text("Untitled album");
        self.album_name_input.set_focus(true);
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some("Name the album, choose photographs, then save".to_owned());
    }

    /// Open the compact, shared-widget metadata sheet for an explicit card.
    /// Nothing is inferred from pixels: people are only labels the user types.
    fn begin_metadata_editor(&mut self, doc: &AutoCommit, photo_id: &str) {
        let Some(photo) = PhotosDoc::library(doc).photo(photo_id).cloned() else {
            self.notice = Some("That photograph is no longer in the library".to_owned());
            return;
        };
        self.metadata_editor = true;
        self.metadata_editor_id = Some(photo.id);
        self.metadata_folder_input
            .set_text(photo.folder.as_deref().unwrap_or_default());
        self.metadata_people_input
            .set_text(&photo.people.join(", "));
        self.metadata_tags_input.set_text(&photo.tags.join(", "));
        self.metadata_captured_at_input
            .set_text(photo.captured_at.as_deref().unwrap_or_default());
        self.metadata_folder_input.set_focus(true);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_editor_focus = 0;
        self.metadata_editor_scroll = 0.0;
        self.metadata_editor_reveal_focus = true;
        self.album_editor = false;
        // The sheet owns the full privacy explanation. Keeping a duplicate
        // in the persistent status band made it wrap below a phone viewport.
        self.notice = None;
    }

    fn cancel_metadata_editor(&mut self) {
        self.metadata_editor = false;
        self.metadata_editor_id = None;
        self.metadata_folder_input.set_focus(false);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_editor_focus = 0;
        self.notice = Some("Photo details discarded".to_owned());
    }

    fn save_metadata_editor(&mut self, doc: &mut AutoCommit) {
        let Some(photo_id) = self.metadata_editor_id.clone() else {
            return;
        };
        let labels = |input: &TextInput| {
            input
                .value()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let optional = |input: &TextInput| {
            let value = input.value().trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        self.commit_library_action(
            doc,
            &crate::actions::PhotosAction::SetMetadata {
                photo_id,
                folder: optional(&self.metadata_folder_input),
                people: labels(&self.metadata_people_input),
                tags: labels(&self.metadata_tags_input),
                captured_at: optional(&self.metadata_captured_at_input),
            },
        );
        self.metadata_editor = false;
        self.metadata_editor_id = None;
        self.metadata_editor_focus = 0;
        self.notice = Some("Shared photo details saved".to_owned());
    }

    fn set_metadata_editor_focus(&mut self, focus: usize) {
        self.metadata_editor_focus = focus.min(5);
        self.metadata_editor_reveal_focus = true;
        self.metadata_folder_input.set_focus(focus == 0);
        self.metadata_people_input.set_focus(focus == 1);
        self.metadata_tags_input.set_focus(focus == 2);
        self.metadata_captured_at_input.set_focus(focus == 3);
    }

    fn begin_edit_album(&mut self, doc: &AutoCommit) {
        let Some(id) = self.active_album.clone() else {
            self.notice = Some("Choose an album to edit".to_owned());
            return;
        };
        let Some(album) = PhotosDoc::library(doc).album(&id).cloned() else {
            self.notice = Some("That album is no longer available".to_owned());
            self.active_album = None;
            return;
        };
        self.album_editor = true;
        self.album_editor_creating = false;
        self.album_editor_id = Some(album.id);
        self.album_name_input.set_text(&album.name);
        self.album_name_input.set_focus(true);
        self.album_editor_members = album.photo_ids.into_iter().collect();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
    }

    fn cancel_album_editor(&mut self) {
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some("Album changes discarded".to_owned());
    }

    fn save_album_editor(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.album_editor_id.clone() else {
            return;
        };
        let name = self.album_name_input.value().trim().to_owned();
        let library = PhotosDoc::library(doc);
        let stale = if self.album_editor_creating {
            library.album(&id).is_some()
        } else {
            library.album(&id).is_none()
        };
        if stale
            || name.is_empty()
            || !self
                .album_editor_members
                .iter()
                .all(|photo| library.photo(photo).is_some())
        {
            self.notice = Some(if stale {
                "Album changed elsewhere; review and save again".to_owned()
            } else {
                "An album needs a name and photographs still in this library".to_owned()
            });
            return;
        }
        let action = crate::actions::PhotosAction::SaveAlbum {
            id: id.clone(),
            name,
            // Preserve the library's stable browse order. HashSet iteration
            // would make two otherwise identical UI edits serialize to
            // arbitrary membership order and create needless collaboration
            // conflicts.
            photo_ids: library
                .photos
                .iter()
                .filter(|photo| self.album_editor_members.contains(&photo.id))
                .map(|photo| photo.id.clone())
                .collect(),
            create: self.album_editor_creating,
        };
        self.commit_library_action(doc, &action);
        self.active_album = Some(id);
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some("Album saved for everyone in this workspace".to_owned());
    }

    fn delete_active_album(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self
            .album_editor_id
            .clone()
            .or_else(|| self.active_album.clone())
        else {
            return;
        };
        self.commit_library_action(doc, &crate::actions::PhotosAction::RemoveAlbum { id });
        self.active_album = None;
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some("Album deleted; photographs remain in the library".to_owned());
    }

    /// Move through the bounded library shelf without changing any shared
    /// state.  Album editing keeps its unsaved membership locally, so paging
    /// must stay available while the sheet is open: otherwise a phone-sized
    /// sheet can leave most of a large library impossible to include.
    fn page_library(&mut self, forward: bool) {
        if forward {
            self.library_offset =
                (self.library_offset + LIBRARY_PAGE_SIZE).min(self.library_total.saturating_sub(1));
        } else {
            self.library_offset = self.library_offset.saturating_sub(LIBRARY_PAGE_SIZE);
        }
    }

    fn can_page_library(&self, forward: bool) -> bool {
        if forward {
            self.library_offset.saturating_add(LIBRARY_PAGE_SIZE) < self.library_total
        } else {
            self.library_offset > 0
        }
    }

    fn library_page_label(&self) -> String {
        if self.library_total == 0 {
            "Showing 0 of 0".to_owned()
        } else {
            let first = self.library_offset + 1;
            let last = (self.library_offset + LIBRARY_PAGE_SIZE).min(self.library_total);
            format!("Showing {first}–{last} of {}", self.library_total)
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "embedded TextInput uses the Widget event contract; keyboard and pointer handling stay beside the album-sheet controls"
    )]
    fn handle_album_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.album_editor {
            return false;
        }
        for (_, button) in &mut self.album_page_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                scale_factor,
                &modifiers,
                doc,
                path,
                handled,
                &mut sink,
                node_id,
                taffy,
            );
        }
        if let Some(hit) = self.album_page_pending.take() {
            match hit {
                AlbumEditorHit::PreviousPage => self.page_library(false),
                AlbumEditorHit::NextPage => self.page_library(true),
                _ => {}
            }
            *handled = true;
            return true;
        }
        let mut sink = None;
        self.album_name_input.handle_event(
            event,
            scale_factor,
            &modifiers,
            doc,
            path,
            handled,
            &mut sink,
            node_id,
            taffy,
        );
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::PageDown) => {
                    self.page_library(true);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::PageUp) => {
                    self.page_library(false);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Tab) => {
                    let steps = if self.album_editor_creating { 3 } else { 4 };
                    self.album_editor_focus = if modifiers.state().shift_key() {
                        (self.album_editor_focus + steps - 1) % steps
                    } else {
                        (self.album_editor_focus + 1) % steps
                    };
                    self.album_name_input
                        .set_focus(self.album_editor_focus == 0);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_album_editor();
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if modifiers.state().control_key() || modifiers.state().super_key() =>
                {
                    self.save_album_editor(doc);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    match (self.album_editor_creating, self.album_editor_focus) {
                        (_, 1) => self.cancel_album_editor(),
                        (false, 2) if self.album_delete_confirm => self.delete_active_album(doc),
                        (false, 2) => self.album_delete_confirm = true,
                        (true, 2) | (false, 3) => self.save_album_editor(doc),
                        _ => {}
                    }
                    *handled = true;
                    return true;
                }
                _ => {}
            }
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            if let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
            {
                match hit {
                    AlbumEditorHit::Save => self.save_album_editor(doc),
                    AlbumEditorHit::Cancel => self.cancel_album_editor(),
                    AlbumEditorHit::Delete => {
                        self.album_delete_confirm = true;
                        self.notice = Some("Press Delete album? again to keep photographs and delete only the album".to_owned());
                    }
                    AlbumEditorHit::ConfirmDelete => self.delete_active_album(doc),
                    _ => {}
                }
                *handled = true;
                return true;
            }
            if let Some((id, _, _)) = self
                .library_cards
                .iter()
                .find(|(_, rect, _)| rect.contains(self.pointer))
                .cloned()
            {
                if !self.album_editor_members.insert(id.clone()) {
                    self.album_editor_members.remove(&id);
                }
                self.album_delete_confirm = false;
                *handled = true;
                return true;
            }
        }
        true
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the metadata sheet embeds four shared TextInputs under the Widget event contract"
    )]
    fn handle_metadata_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.metadata_editor {
            return false;
        }
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Tab) => {
                    self.set_metadata_editor_focus(if modifiers.state().shift_key() {
                        (self.metadata_editor_focus + 5) % 6
                    } else {
                        (self.metadata_editor_focus + 1) % 6
                    });
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_metadata_editor();
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if modifiers.state().control_key() || modifiers.state().super_key() =>
                {
                    self.save_metadata_editor(doc);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == 4 => {
                    self.cancel_metadata_editor();
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == 5 => {
                    self.save_metadata_editor(doc);
                    *handled = true;
                    return true;
                }
                _ => {}
            }
        }
        // A wheel over the sheet scrolls its field list (clamped at paint,
        // where the content height is known).
        if let winit::event::WindowEvent::MouseWheel { delta, .. } = event
            && self.metadata_editor_body.contains(self.pointer)
        {
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 40.0,
                // Physical pixels, like every pointer position this app
                // converts; without the division a 2× display scrolls twice
                // the trackpad distance.
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y / scale_factor,
            };
            self.metadata_editor_scroll = (self.metadata_editor_scroll - dy).max(0.0);
            *handled = true;
            return true;
        }
        // A press outside the field list's visible region belongs to the
        // pinned footer (or the scrim), never to an input scrolled under it.
        let press_outside_body = matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                ..
            }
        ) && !self.metadata_editor_body.contains(self.pointer);
        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
        ] {
            if press_outside_body {
                break;
            }
            let mut sink = None;
            input.handle_event(
                event,
                scale_factor,
                &modifiers,
                doc,
                path,
                handled,
                &mut sink,
                node_id,
                taffy,
            );
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && let Some((hit, _)) = self
            .metadata_editor_hits
            .iter()
            .find(|(_, rect)| rect.contains(self.pointer))
            .copied()
        {
            match hit {
                MetadataEditorHit::Save => self.save_metadata_editor(doc),
                MetadataEditorHit::Cancel => self.cancel_metadata_editor(),
            }
            *handled = true;
        }
        true
    }

    /// Perform a device-local operation and surface failures to canonical
    /// operation dispatch. A local notice alone is not an invocation result.
    fn perform_effect(
        &mut self,
        operation: crate::actions::PhotosOperation,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        match operation {
            // Resource/search routing has already brought this Photos
            // instance forward. Viewing does not alter document or OS state.
            crate::actions::PhotosOperation::Open { path: None } => {
                self.import_content = Some(LiveContent::read(doc));
                self.picker_stale = false;
                self.picker = Some(
                    ui::portal::PickRequest::open(&tr("photos-open-title"), &tr("photos-open"))
                        .thread_name("photos-portal-open")
                        .filter(
                            ui::portal::FileFilter::new(&tr("photos-filter-images"))
                                .mimetype("image/png")
                                .mimetype("image/jpeg")
                                .mimetype("image/webp"),
                        )
                        .all_files()
                        .send(),
                );
            }
            crate::actions::PhotosOperation::Copy => {
                self.copy_to_clipboard();
            }
            crate::actions::PhotosOperation::ShareReference { resource_id } => {
                let exists = PhotosDoc::image(doc)
                    .is_some_and(|image| crate::actions::resource_id(&image) == resource_id)
                    || PhotosDoc::library(doc)
                        .photos
                        .iter()
                        .any(|photo| photo.id == resource_id);
                if exists {
                    // This is deliberately a local clipboard handoff, not an
                    // invented upload or recipient-delivery success. The
                    // address identifies either the open editor image or an
                    // unselected, addressable library card.
                    ui::clipboard_copy(&format!("qos://resource/{resource_id}"));
                    self.notice = Some(tr("photos-share-copied"));
                } else {
                    let error = tr("photos-share-failed");
                    self.notice = Some(error.clone());
                    return Err(error);
                }
            }
            crate::actions::PhotosOperation::Save { path } => {
                if let Some(open) = PhotosDoc::image(doc) {
                    self.save_to(path.as_deref().unwrap_or(&Self::suggested_save_path(&open)));
                }
            }
            crate::actions::PhotosOperation::ExportEditedContent { .. } => {
                return Err("edited content export requires lifecycle dispatch".into());
            }
            crate::actions::PhotosOperation::View
            | crate::actions::PhotosOperation::ViewAlbum { .. }
            | crate::actions::PhotosOperation::ViewFolder { .. }
            | crate::actions::PhotosOperation::ViewPerson { .. }
            | crate::actions::PhotosOperation::ViewTag { .. }
            | crate::actions::PhotosOperation::Open { path: Some(_) } => {}
        }
        Ok(())
    }

    /// Navigate to an extant stable album id without producing a CRDT action.
    /// This is deliberately separate from `perform_effect`: callers need a
    /// useful stale-address error instead of silently falling back to all
    /// photographs if a collaborator removed the album after search indexed
    /// it.
    fn view_album(&mut self, doc: &AutoCommit, album_id: &str) -> Result<(), String> {
        if PhotosDoc::library(doc).album(album_id).is_none() {
            return Err("that album is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = Some(album_id.to_owned());
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.library_offset = 0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing selected album".to_owned());
        Ok(())
    }

    /// Enter a metadata-derived library view after verifying the label still
    /// exists. An old global-search result therefore fails honestly when a
    /// collaborator removes the final matching photograph.
    fn view_metadata_filter(
        &mut self,
        doc: &AutoCommit,
        folder: Option<String>,
        person: Option<String>,
        tag: Option<String>,
    ) -> Result<(), String> {
        let library = PhotosDoc::library(doc);
        let exists = match (&folder, &person, &tag) {
            (Some(folder), None, None) => library
                .photos
                .iter()
                .any(|photo| photo.folder.as_deref() == Some(folder)),
            (None, Some(person), None) => library
                .photos
                .iter()
                .any(|photo| photo.people.iter().any(|value| value == person)),
            (None, None, Some(tag)) => library
                .photos
                .iter()
                .any(|photo| photo.tags.iter().any(|value| value == tag)),
            _ => false,
        };
        if !exists {
            return Err("that metadata view is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = folder;
        self.active_person = person;
        self.active_tag = tag;
        self.library_offset = 0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing shared photo metadata view".to_owned());
        Ok(())
    }

    /// Re-derive the rendering if the path or the edits have moved.
    fn refresh(&mut self, doc: &AutoCommit) {
        let Some(open) = PhotosDoc::image(doc) else {
            self.rendered = None;
            self.viewer = None;
            self.original = None;
            self.canvas_state = if self.notice.is_some() {
                CanvasState::Error
            } else {
                CanvasState::Empty
            };
            return;
        };
        let edits = PhotosDoc::edits(doc);

        // Load the original if this is a picture we have not decoded yet. A
        // peer may have opened it, so this is not only the local Open path.
        let identity = decode_key(&open);
        if self.original_identity.as_ref() != Some(&identity) || self.original.is_none() {
            let key = decode_key(&open);
            if self.original_decode_failed.as_deref() == Some(&key) {
                self.original = None;
                self.rendered = None;
                self.viewer = None;
                self.canvas_state = CanvasState::Error;
                return;
            }
            let bytes = crate::doc::bytes_of(&open);
            if let Some(img) = bytes.as_deref().and_then(pixels::decode) {
                self.original = Some((open.path, img));
                self.original_identity = Some(identity.clone());
                self.original_decode_failed = None;
            } else {
                self.original = None;
                self.rendered = None;
                self.viewer = None;
                if bytes.is_some() {
                    self.original_decode_failed = Some(key);
                    self.canvas_state = CanvasState::Error;
                } else {
                    self.canvas_state = CanvasState::Loading;
                }
                return;
            }
        }
        let Some((_, original)) = self.original.as_ref() else {
            return;
        };

        let unchanged = self
            .rendered
            .as_ref()
            .is_some_and(|r| r.identity == identity && r.edits == edits);
        if unchanged {
            return;
        }

        let rgba = pixels::render(original, edits.crop, edits.rotate_quarters);
        let image = to_peniko(&rgba);
        // `ImageViewer` has no in-place image swap yet, so an edit rebuilds
        // the widget — which also resets zoom/pan to fit-to-viewport. For
        // "crop to view" that is the right outcome anyway: the new picture is
        // exactly what was framed, so showing it at zoom 1 is showing what was
        // asked for.
        self.viewer = Some(ImageViewer::new(image));
        self.canvas_state = CanvasState::Ready;
        self.rendered = Some(Rendered {
            identity,
            edits,
            rgba,
        });
    }

    /// Drain a click that a button posted, and act on it.
    fn drain_pending(&mut self, doc: &mut AutoCommit) {
        let Some(hit) = self.pending.take() else {
            return;
        };
        let edits = PhotosDoc::edits(doc);
        match hit {
            Hit::Open => {
                let _ =
                    self.perform_effect(crate::actions::PhotosOperation::Open { path: None }, doc);
            }
            Hit::Library => {
                self.library_mode = true;
                self.active_album = None;
                self.active_folder = None;
                self.active_person = None;
                self.active_tag = None;
                self.notice = None;
            }
            Hit::RotateLeft => self.commit_edits(doc, edits.rotated_left()),
            Hit::RotateRight => self.commit_edits(doc, edits.rotated_right()),
            Hit::CropToView => {
                let view = self.visible_rect();
                // Cropping to the whole image is a no-op; say so rather than
                // writing an edit that changes nothing.
                if view.is_full() {
                    self.notice = Some(tr("photos-crop-noop"));
                } else {
                    self.commit_edits(doc, edits.cropped_from_view(view));
                }
            }
            Hit::Reset => self.commit_edits(doc, Edits::default()),
            Hit::Copy => {
                let _ = self.perform_effect(crate::actions::PhotosOperation::Copy, doc);
            }
            Hit::Share => {
                if let Some(image) = PhotosDoc::image(doc) {
                    let _ = self.perform_effect(
                        crate::actions::PhotosOperation::ShareReference {
                            resource_id: crate::actions::resource_id(&image),
                        },
                        doc,
                    );
                } else {
                    self.notice = Some(tr("photos-share-failed"));
                }
            }
            Hit::Save => {
                let _ =
                    self.perform_effect(crate::actions::PhotosOperation::Save { path: None }, doc);
            }
            Hit::More => {
                self.compact_more_open = !self.compact_more_open;
                self.compact_more_focus = 0;
            }
        }
    }

    /// Synthetic semantics are necessary because Photos composes its toolbar
    /// directly into the root layout instead of exposing it through
    /// [`Widget::children`]. Keep the target based on the stable toolbar key,
    /// not its visual index: the narrow layout changes labels, but an assistive
    /// technology holding "Rotate right" must keep reaching that action.
    fn toolbar_semantics(&self) -> AccessNode {
        let visible: std::collections::HashSet<_> = self
            .toolbar_slots()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Item(index) => Some(*index),
                _ => None,
            })
            .collect();
        let mut children: Vec<_> = self
            .buttons
            .iter()
            .zip(TOOLBAR)
            .enumerate()
            .filter(|(index, _)| visible.contains(index))
            .map(|(_, ((_, button), (_, key, _, _)))| {
                AccessNode::new(Role::Button)
                    .with_name(tr(key))
                    .with_state(AccessState {
                        focusable: button.is_focusable(),
                        focused: button.is_focused(),
                        disabled: !button.is_enabled(),
                        ..Default::default()
                    })
                    .with_bounds(button.get_rect())
                    .with_action_target(format!("photos:toolbar:{key}"))
            })
            .collect();
        if self.compact_more_open && self.toolbar_slots() == COMPACT_SLOTS {
            let menu_children = [
                ("photos-crop", "photos-crop"),
                ("photos-reset", "photos-reset"),
                ("photos-copy", "photos-copy"),
                ("photos-share", "photos-share"),
                ("photos-save", "photos-save"),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (target_key, label_key))| {
                let menu_hit = COMPACT_MORE_ENTRIES.get(index).copied();
                let disabled = TOOLBAR
                    .iter()
                    .position(|(_, key, _, _)| *key == target_key)
                    .and_then(|index| self.buttons.get(index))
                    .is_none_or(|(_, button)| !button.is_enabled());
                AccessNode::new(Role::MenuItem)
                    .with_name(tr(label_key))
                    .with_state(AccessState {
                        focusable: !disabled,
                        focused: index == self.compact_more_focus,
                        disabled,
                        ..Default::default()
                    })
                    .with_bounds(
                        self.compact_more_hits
                            .iter()
                            .find(|(hit, _)| Some(*hit) == menu_hit)
                            .map_or(Rect::ZERO, |(_, bounds)| *bounds),
                    )
                    .with_action_target(format!("photos:toolbar:{target_key}"))
            })
            .collect();
            children.push(
                AccessNode::new(Role::Menu)
                    .with_name(tr("photos-more"))
                    .with_children(menu_children),
            );
        }
        AccessNode::new(Role::Toolbar)
            .with_name(tr("photos-tools"))
            .with_bounds(self.toolbar_rect)
            .with_hashed_key("photos:toolbar")
            .with_children(children)
    }

    /// The canvas has no intrinsic source name — it is raw pixel data — so the
    /// app supplies the file name and current edit state that a sighted person
    /// gets from the status bar. The status remains a polite live region for
    /// copy/save/open failures without stealing keyboard focus.
    #[allow(
        clippy::too_many_lines,
        reason = "the synthetic canvas accessibility tree keeps each visual hit target beside its stable semantic route"
    )]
    fn accessibility_tree(&self) -> AccessNode {
        let image_label = || {
            self.original.as_ref().map_or_else(
                || tr("photos-image"),
                |(path, _)| path.rsplit('/').next().unwrap_or(path).to_owned(),
            )
        };
        let (image_name, image_description) = match self.rendered.as_ref() {
            Some(rendered) if rendered.edits.is_identity() => {
                (image_label(), tr("photos-image-help"))
            }
            Some(_) => (
                format!("{} — {}", image_label(), tr("photos-edited")),
                tr("photos-image-help"),
            ),
            None => self.empty_state_text(),
        };
        let canvas = if self.library_mode {
            let mut cards: Vec<_> = self
                .library_cards
                .iter()
                .filter_map(|(id, bounds, favorite)| {
                    let item = self.library_items.iter().find(|item| item.id == *id)?;
                    let name = &item.name;
                    let open_target = format!("photos:library-photo:{id}");
                    let mut children = vec![if self.album_editor {
                        AccessNode::new(Role::ToggleButton)
                            .with_name(name.clone())
                            .with_description("Included in this album")
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                checked: Some(self.album_editor_members.contains(id)),
                                ..Default::default()
                            })
                            .with_action_target(open_target)
                    } else {
                        AccessNode::new(Role::Button)
                            .with_name(name.clone())
                            .with_description(tr("photos-open"))
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(open_target)
                    }];
                    if !self.album_editor {
                        let favorite_target = format!("photos:library-favorite:{id}");
                        // The same rects `draw_library` paints and the pointer
                        // hit-tests: one description for the action row.
                        let actions = card_action_controls(*bounds);
                        children.push(
                            AccessNode::new(Role::ToggleButton)
                                .with_name("Favorite")
                                .with_bounds(actions.favorite)
                                .with_state(AccessState {
                                    focusable: true,
                                    checked: Some(*favorite),
                                    ..Default::default()
                                })
                                .with_action_target(favorite_target),
                        );
                        let confirmed = self.library_remove_confirm.as_deref() == Some(id);
                        children.push(
                            AccessNode::new(Role::Button)
                                .with_name(if confirmed {
                                    format!("Confirm removing {name} from library")
                                } else {
                                    format!("Remove {name} from library")
                                })
                                .with_description("Removes this photograph from every album")
                                .with_bounds(actions.remove)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(format!("photos:library-remove:{id}")),
                        );
                        children.push(
                            AccessNode::new(Role::Button)
                                .with_name(format!("Edit details for {name}"))
                                .with_description(
                                    "Edit shared folder, people, tags, and capture date",
                                )
                                .with_bounds(actions.details)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(format!("photos:library-details:{id}")),
                        );
                    }
                    Some(
                        AccessNode::new(Role::ListItem)
                            .with_name(name.clone())
                            .with_bounds(*bounds)
                            .with_hashed_key(format!("photos:library-item:{id}"))
                            .with_children(children),
                    )
                })
                .collect();
            cards.extend(self.album_cards.iter().map(|(id, bounds)| {
                let label = id
                    .as_ref()
                    .and_then(|id| {
                        self.library_albums
                            .iter()
                            .find(|(album_id, _, _)| album_id == id)
                    })
                    .map_or_else(
                        || "All photos".to_owned(),
                        |(_, name, count)| format!("{name} ({count})"),
                    );
                let target = format!("photos:library-album:{}", id.as_deref().unwrap_or("all"));
                AccessNode::new(Role::ToggleButton)
                    .with_name(label)
                    .with_bounds(*bounds)
                    .with_state(AccessState {
                        focusable: true,
                        checked: Some(id.as_ref() == self.active_album.as_ref()),
                        ..Default::default()
                    })
                    .with_action_target(target)
            }));
            cards.extend(
                self.metadata_filter_cards
                    .iter()
                    .map(|(folder, person, bounds)| {
                        let label = match (folder, person) {
                            (Some(folder), None) => format!("Folder: {folder}"),
                            (None, Some(person)) => format!("People: {person}"),
                            _ => "All metadata".to_owned(),
                        };
                        AccessNode::new(Role::ToggleButton)
                            .with_name(label)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                checked: Some(
                                    folder.as_ref() == self.active_folder.as_ref()
                                        && person.as_ref() == self.active_person.as_ref()
                                        && self.active_album.is_none(),
                                ),
                                ..Default::default()
                            })
                            .with_action_target(format!(
                                "photos:library-metadata:{}:{}",
                                folder.as_deref().unwrap_or_default(),
                                person.as_deref().unwrap_or_default()
                            ))
                    }),
            );
            if self.metadata_editor {
                for (name, input) in [
                    ("Folder", &self.metadata_folder_input),
                    ("People", &self.metadata_people_input),
                    ("Tags", &self.metadata_tags_input),
                    ("Captured", &self.metadata_captured_at_input),
                ] {
                    cards.push(
                        AccessNode::new(Role::TextInput)
                            .with_name(name)
                            .with_value(input.value())
                            .with_bounds(input.get_rect())
                            .with_state(AccessState {
                                focusable: true,
                                focused: input.is_focused(),
                                ..Default::default()
                            })
                            .with_hashed_key(format!("photos:metadata:{name}")),
                    );
                }
                for (hit, bounds) in &self.metadata_editor_hits {
                    let (name, target) = match hit {
                        MetadataEditorHit::Save => ("Save photo details", "photos:metadata:save"),
                        MetadataEditorHit::Cancel => {
                            ("Cancel photo details", "photos:metadata:cancel")
                        }
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
            } else if self.album_editor {
                cards.push(
                    AccessNode::new(Role::TextInput)
                        .with_name("Album name")
                        .with_value(self.album_name_input.value())
                        .with_bounds(self.album_name_input.get_rect())
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.album_name_input.is_focused(),
                            ..Default::default()
                        })
                        .with_hashed_key("photos:album-editor:name"),
                );
                for (hit, button) in &self.album_page_buttons {
                    let (name, target) = match hit {
                        AlbumEditorHit::PreviousPage => (
                            "Show previous photographs",
                            "photos:album-editor:previous-page",
                        ),
                        AlbumEditorHit::NextPage => {
                            ("Show next photographs", "photos:album-editor:next-page")
                        }
                        _ => continue,
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_description(self.library_page_label())
                            .with_bounds(button.get_rect())
                            .with_state(AccessState {
                                focusable: button.is_enabled(),
                                focused: button.is_focused(),
                                disabled: !button.is_enabled(),
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                for (hit, bounds) in &self.album_editor_hits {
                    let (name, target) = match hit {
                        AlbumEditorHit::Save => ("Save album", "photos:album-editor:save"),
                        AlbumEditorHit::Cancel => {
                            ("Cancel album editing", "photos:album-editor:cancel")
                        }
                        AlbumEditorHit::Delete => ("Delete album", "photos:album-editor:delete"),
                        AlbumEditorHit::ConfirmDelete => (
                            "Confirm deleting album",
                            "photos:album-editor:confirm-delete",
                        ),
                        AlbumEditorHit::New
                        | AlbumEditorHit::Edit
                        | AlbumEditorHit::PreviousPage
                        | AlbumEditorHit::NextPage => continue,
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
            } else {
                for (hit, bounds) in &self.album_editor_hits {
                    let (name, target) = match hit {
                        AlbumEditorHit::New => ("New album", "photos:album-editor:new"),
                        AlbumEditorHit::Edit => ("Edit selected album", "photos:album-editor:edit"),
                        _ => continue,
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
            }
            AccessNode::new(Role::List)
                .with_name(tr("photos-library"))
                .with_description(format!("{} photographs", self.library_total))
                .with_bounds(self.canvas_rect)
                .with_hashed_key("photos:library-grid")
                .with_children(cards)
        } else {
            AccessNode::new(Role::Image)
                .with_name(image_name)
                .with_description(image_description)
                .with_bounds(self.canvas_rect)
                .with_hashed_key("photos:canvas")
        };
        let mut children = vec![self.toolbar_semantics(), canvas];
        if !self.status.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.status.clone())
                    .with_live(Politeness::Polite)
                    .with_bounds(self.status_rect)
                    .with_hashed_key("photos:status"),
            );
        }
        AccessNode::new(Role::Group)
            .with_name(tr("photos-app"))
            .with_bounds(self.rect)
            .with_semantic_key("photos")
            .with_children(children)
    }

    /// Dispatch a toolbar action requested by assistive technology through the
    /// identical `drain_pending` path used by pointer and keyboard activation.
    fn perform_toolbar_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        doc: &mut AutoCommit,
    ) -> bool {
        let Some(key) = target.strip_prefix("photos:toolbar:") else {
            return false;
        };
        let Some((index, (hit, _, _, _))) = TOOLBAR
            .iter()
            .enumerate()
            .find(|(_, (_, toolbar_key, _, _))| *toolbar_key == key)
        else {
            return false;
        };
        match action {
            AccessAction::Focus => {
                for (_, button) in &mut self.buttons {
                    button.set_focus(false);
                }
                if let Some((_, button)) = self.buttons.get_mut(index) {
                    button.set_focus(true);
                }
                true
            }
            AccessAction::Activate
                if self.buttons.get(index).is_some_and(|(_, b)| b.is_enabled()) =>
            {
                self.pending.set(Some(*hit));
                self.drain_pending(doc);
                true
            }
            _ => false,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the library semantics mirror every visible card, editor, and metadata filter target"
    )]
    fn perform_library_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        doc: &mut AutoCommit,
    ) -> bool {
        if let Some(command) = target.strip_prefix("photos:album-editor:") {
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "new" => self.begin_new_album(doc),
                "edit" => self.begin_edit_album(doc),
                "previous-page" if self.can_page_library(false) => self.page_library(false),
                "next-page" if self.can_page_library(true) => self.page_library(true),
                "save" => self.save_album_editor(doc),
                "cancel" => self.cancel_album_editor(),
                "delete" => self.album_delete_confirm = true,
                "confirm-delete" => self.delete_active_album(doc),
                _ => return false,
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:metadata:") {
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "save" => self.save_metadata_editor(doc),
                "cancel" => self.cancel_metadata_editor(),
                _ => return false,
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:library-album:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            self.active_album = (album_id != "all").then(|| album_id.to_owned());
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.library_offset = 0;
            return true;
        }
        if let Some(id) = target.strip_prefix("photos:library-details:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                self.begin_metadata_editor(doc, id);
            }
            return true;
        }
        if let Some(values) = target.strip_prefix("photos:library-metadata:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let (folder, person) = values.split_once(':').unwrap_or((values, ""));
            self.active_album = None;
            self.active_folder = (!folder.is_empty()).then(|| folder.to_owned());
            self.active_person = (!person.is_empty()).then(|| person.to_owned());
            self.active_tag = None;
            self.library_offset = 0;
            return true;
        }
        let (photo_id, set_favorite, remove) =
            if let Some(id) = target.strip_prefix("photos:library-photo:") {
                (id, false, false)
            } else if let Some(id) = target.strip_prefix("photos:library-favorite:") {
                (id, true, false)
            } else if let Some(id) = target.strip_prefix("photos:library-remove:") {
                (id, false, true)
            } else {
                return false;
            };
        if !matches!(action, AccessAction::Activate) {
            return matches!(action, AccessAction::Focus);
        }
        let Some(item) = self.library_items.iter().find(|item| item.id == photo_id) else {
            return false;
        };
        let favorite = item.favorite;
        if remove {
            if self.library_remove_confirm.as_deref() == Some(photo_id) {
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::RemovePhoto {
                        photo_id: photo_id.to_owned(),
                    },
                );
                self.library_remove_confirm = None;
                self.notice = Some("Photograph removed from the shared library".to_owned());
            } else {
                self.library_remove_confirm = Some(photo_id.to_owned());
                self.notice = Some(
                    "Activate Remove again to confirm; it will also leave every album".to_owned(),
                );
            }
        } else if set_favorite {
            self.commit_library_action(
                doc,
                &crate::actions::PhotosAction::SetFavorite {
                    photo_id: photo_id.to_owned(),
                    favorite: !favorite,
                },
            );
        } else if self.album_editor {
            if !self.album_editor_members.insert(photo_id.to_owned()) {
                self.album_editor_members.remove(photo_id);
            }
        } else {
            self.commit_library_action(
                doc,
                &crate::actions::PhotosAction::SelectPhoto {
                    photo_id: photo_id.to_owned(),
                },
            );
            self.library_mode = false;
        }
        true
    }

    /// Collect a chosen file from the portal, if one has arrived.
    fn drain_picker(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.picker.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.picker = None;
                self.import_content = None;
                if self.picker_stale {
                    // The chooser already ran. Settle its real selection outcome
                    // without importing into content changed since dispatch.
                    let message = format!(
                        "Selected {}; import was invalidated by changed document content",
                        path.display()
                    );
                    self.notice = Some(message.clone());
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.resource_result = Some(qos_actions::InvocationUpdate::Failed {
                            invocation,
                            error: qos_actions::ActionFailure {
                                code: "photos.stale_selection".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        });
                    }
                    return;
                }
                let path = path.to_string_lossy().into_owned();
                self.open_path(doc, &path);
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(if self.notice.is_none() {
                        qos_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: qos_actions::ActionOutput::default(),
                        }
                    } else {
                        qos_actions::InvocationUpdate::Failed {
                            invocation,
                            error: qos_actions::ActionFailure {
                                code: "photos.open_failed".into(),
                                message: tr("photos-open-failed"),
                                details: None,
                                retryable: false,
                            },
                        }
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.picker = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(qos_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("photo selection was cancelled".into()),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "photos.portal_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picker = None,
        }
    }

    /// Which toolbar buttons are usable right now. Everything but Open needs
    /// an image; Reset additionally needs something to undo.
    fn enabled(hit: Hit, has_image: bool, edited: bool) -> bool {
        match hit {
            Hit::Open | Hit::Library => true,
            Hit::Reset => has_image && edited,
            _ => has_image,
        }
    }

    /// The status line: the file, its edited size, and any notice.
    fn status_text(&self, doc: &AutoCommit) -> String {
        let Some(open) = PhotosDoc::image(doc) else {
            return self.notice.clone().unwrap_or_default();
        };
        let edits = PhotosDoc::edits(doc);
        let (w, h) = open.edited_size(&edits);
        let mut s = format!("{}  ·  {w} × {h}", open.name);
        if !edits.is_identity() {
            s.push_str("  ·  ");
            s.push_str(&tr("photos-edited"));
        }
        if let Some(n) = &self.notice {
            s.push_str("  ·  ");
            s.push_str(n);
        }
        s
    }

    fn empty_state_text(&self) -> (String, String) {
        match self.canvas_state {
            CanvasState::Loading => (tr("photos-loading-title"), tr("photos-loading-body")),
            CanvasState::Error => (tr("photos-error-title"), tr("photos-error-body")),
            CanvasState::Empty | CanvasState::Ready => {
                (tr("photos-empty-title"), tr("photos-empty-body"))
            }
        }
    }

    /// Lay out the toolbar strip: one taffy node per [`SLOTS`] entry (a
    /// button, a separator rule, or the greedy spacer), wrapped in the bar
    /// row itself. Split out of `build_layout` to keep that dispatch under
    /// the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    fn build_toolbar_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut bar_children = Vec::new();
        for slot in self.toolbar_slots() {
            let node = match *slot {
                Slot::Item(n) => match self.buttons.get_mut(n) {
                    Some((_, button)) => button.build_layout(taffy, theme, node_map),
                    // Unreachable: SLOTS only ever indexes within self.buttons.
                    None => taffy.new_leaf(Style::default()).unwrap(),
                },
                Slot::Separator => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::length(SEP_W),
                            height: Dimension::length(self.toolbar_btn_h * 0.6),
                        },
                        flex_shrink: 0.0,
                        ..Default::default()
                    })
                    .unwrap(),
                // Soaks the residual width so the export group sits hard
                // right instead of leaving a third of the strip bare.
                Slot::Spacer => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::length(0.0),
                            height: Dimension::length(0.0),
                        },
                        flex_grow: 1.0,
                        flex_shrink: 1.0,
                        ..Default::default()
                    })
                    .unwrap(),
            };
            bar_children.push(node);
        }
        taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        // `bar_h` at the scale the capsules were built for:
                        // the authored BAR_H at 1×.
                        height: Dimension::length(self.toolbar_btn_h + (BAR_H - BTN_H)),
                    },
                    flex_shrink: 0.0,
                    align_items: Some(AlignItems::Center),
                    gap: Size {
                        width: LengthPercentage::length(BTN_GAP),
                        height: LengthPercentage::length(0.0),
                    },
                    padding: taffy::Rect {
                        left: LengthPercentage::length(PAD),
                        right: LengthPercentage::length(PAD),
                        top: LengthPercentage::length(0.0),
                        bottom: LengthPercentage::length(0.0),
                    },
                    ..Default::default()
                },
                &bar_children,
            )
            .unwrap()
    }

    /// Lay out the canvas: the `ImageViewer` if a photograph is open, else a
    /// placeholder leaf sized to soak the remaining height. `flex_basis: 0`
    /// with `min_size: 0` is what stops a large image's intrinsic size from
    /// pushing the layout wider than the window — the failure mode where
    /// content is silently clipped because the container grew past its
    /// parent instead of the content being scaled to fit. Split out of
    /// `build_layout` to keep that dispatch under the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    fn build_canvas_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let canvas = match self.viewer.as_mut() {
            Some(v) => v.build_layout(taffy, theme, node_map),
            None => taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    ..Default::default()
                })
                .unwrap(),
        };
        taffy
            .set_style(
                canvas,
                Style {
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::auto(),
                    },
                    min_size: Size {
                        width: Dimension::length(0.0),
                        height: Dimension::length(0.0),
                    },
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: Dimension::length(0.0),
                    margin: taffy::Rect {
                        left: LengthPercentageAuto::length(0.0),
                        right: LengthPercentageAuto::length(0.0),
                        top: LengthPercentageAuto::length(CANVAS_INSET),
                        bottom: LengthPercentageAuto::length(CANVAS_INSET),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        canvas
    }

    /// Lay out the status band: zero height with nothing to say (reserving
    /// the band anyway would leave a rule and a strip of empty chrome under
    /// an empty state, which reads as a control that failed to draw), else a
    /// fixed band. Split out of `build_layout` to keep that dispatch under
    /// the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "this taffy node is freshly built with a fixed, valid style"
    )]
    fn build_status_node(&self, taffy: &mut TaffyTree<()>, text_scale: f32) -> NodeId {
        taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(if self.status.is_empty() {
                        0.0
                    } else {
                        status_h(text_scale)
                    }),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the grid paint keeps its pixel geometry and bounded materialisation together"
    )]
    /// Render the visible part of the library; the remainder stays reachable
    /// through cursor-paged search and canonical resources.
    fn draw_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.album_cards.clear();
        self.metadata_filter_cards.clear();
        self.library_details_cards.clear();
        if self.library_items.is_empty() {
            EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.canvas_rect,
                Some(Icon::Image),
                &tr("photos-library-empty-title"),
                Some(&tr("photos-library-empty-body")),
                None,
                Point::ZERO,
            );
            // Empty collections are useful before the first import too; keep
            // the same New album affordance available rather than stranding
            // the user behind an empty-state dead end.
            self.draw_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        let bounds = self.canvas_rect;
        // Enlarged text needs its own density rule. Five desktop cards leave
        // only a few words of metadata per card at 200%, which makes labels
        // overlap their independent destructive controls.
        let large_text = theme.text_scale > 1.25;
        let columns = library_column_count(bounds.width(), large_text, self.metadata_editor);
        let gap = if columns <= 2 { 8.0 } else { 12.0 };
        let card_w =
            ((bounds.width() - f64::from(PAD * 2.0) - gap * columns.saturating_sub(1) as f64)
                / columns as f64)
                .max(1.0);
        let album_h = if self.library_albums.is_empty() || large_text {
            0.0
        } else {
            38.0
        };
        let heading_y = bounds.y0 + f64::from(PAD);
        text_renderer.draw_text_weighted(
            scene,
            &tr("photos-library"),
            18.0,
            theme.text_primary,
            transform * Affine::translate((bounds.x0 + f64::from(PAD), heading_y)),
            Some((bounds.width() - f64::from(PAD * 2.0)) as f32),
            TextWeight::Medium,
        );
        let count = if large_text {
            format!("{} photographs", self.library_total)
        } else if self.library_total == 0 {
            "0".to_owned()
        } else {
            let first = self.library_offset + 1;
            let last = (self.library_offset + self.library_items.len()).min(self.library_total);
            format!("{first}–{last} / {}", self.library_total)
        };
        // Right-align the page count to the grid's right edge from its measured
        // width (it used to start at a fixed 100 px allowance and end wherever
        // the digits happened to stop, ~60 px short of the cards).
        let count_budget: f32 = if large_text { 180.0 } else { 100.0 };
        let (count_w, _) =
            text_renderer.measure_weighted(&count, 13.0, Some(count_budget), TextWeight::Regular);
        let count_x = bounds.x1 - f64::from(PAD) - f64::from(count_w.min(count_budget));
        text_renderer.draw_text_weighted(
            scene,
            &count,
            13.0,
            theme.text_dim,
            transform * Affine::translate((count_x, heading_y + 3.0)),
            Some(count_budget),
            TextWeight::Regular,
        );
        if !self.library_albums.is_empty() && !large_text {
            let mut x = bounds.x0 + f64::from(PAD);
            let y = heading_y + 24.0;
            let mut albums = vec![(None, format!("All photos ({})", self.library_total))];
            albums.extend(
                self.library_albums
                    .iter()
                    .map(|(id, name, count)| (Some(id.clone()), format!("{name} ({count})"))),
            );
            for (album_id, label) in albums {
                let width = (label.chars().count() as f64 * 7.2 + 18.0).min(150.0);
                if x + width > bounds.x1 - f64::from(PAD) {
                    break;
                }
                let chip = Rect::new(x, y, x + width, y + 26.0);
                let selected = album_id == self.active_album;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if selected {
                        theme.tokens.colors.neutral[3]
                    } else {
                        theme.tokens.colors.surfaces.raised
                    },
                    None,
                    &chip,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(chip.x0, chip.y1 - 1.0, chip.x1, chip.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &label,
                    12.0,
                    theme.text_primary,
                    transform * Affine::translate((chip.x0 + 8.0, chip.y0 + 6.0)),
                    Some((width - 16.0) as f32),
                    TextWeight::Regular,
                );
                self.album_cards.push((album_id, chip));
                x += width + 8.0;
            }
        }
        // Metadata filters are deliberately in the shelf rather than hidden
        // behind global search: folders and people are an everyday browse
        // route.  The six-per-kind cap keeps this compact; every other label
        // remains available through the canonical resource/search surface.
        let header_h = if large_text { 72.0 } else { 31.0 };
        let filter_y = heading_y + header_h + album_h;
        let mut filter_x = bounds.x0 + f64::from(PAD);
        let compact_filters = columns <= 2;
        let mut filters = vec![(
            None,
            None,
            if compact_filters {
                "All".to_owned()
            } else {
                "All metadata".to_owned()
            },
        )];
        if large_text {
            // Other durable metadata views remain available through Search
            // and their own stable resource actions.  Avoid a dense chip row
            // competing with 200%-scaled library titles.
        } else if compact_filters {
            // A phone has one shelf row. Show one folder *and* one explicit
            // people label rather than filling it with folders and hiding the
            // privacy-controlled people route.
            if let Some((folder, count)) = self.library_folders.first() {
                filters.push((
                    Some(folder.clone()),
                    None,
                    format!("{} ({count})", compact_label(folder, 8)),
                ));
            }
            if let Some((person, count)) = self.library_people.first() {
                filters.push((
                    None,
                    Some(person.clone()),
                    format!("{} ({count})", compact_label(person, 8)),
                ));
            }
        } else {
            filters.extend(self.library_folders.iter().map(|(folder, count)| {
                (
                    Some(folder.clone()),
                    None,
                    format!("Folder: {folder} ({count})"),
                )
            }));
            filters.extend(self.library_people.iter().map(|(person, count)| {
                (
                    None,
                    Some(person.clone()),
                    format!("People: {person} ({count})"),
                )
            }));
        }
        for (folder, person, label) in filters {
            let width = (label.chars().count() as f64 * 6.6 + 18.0).clamp(
                if compact_filters { 54.0 } else { 92.0 },
                if compact_filters { 110.0 } else { 196.0 },
            );
            if filter_x + width > bounds.x1 - f64::from(PAD) {
                break;
            }
            let chip_h = if large_text { 44.0 } else { 28.0 };
            let chip = Rect::new(filter_x, filter_y, filter_x + width, filter_y + chip_h);
            let selected = folder.as_ref() == self.active_folder.as_ref()
                && person.as_ref() == self.active_person.as_ref()
                && self.active_album.is_none();
            scene.fill(
                Fill::NonZero,
                transform,
                if selected {
                    theme.tokens.colors.accent[6]
                } else {
                    theme.tokens.colors.surfaces.raised
                },
                None,
                &chip,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(chip.x0, chip.y1 - 1.0, chip.x1, chip.y1),
            );
            text_renderer.draw_text_weighted(
                scene,
                &label,
                12.0,
                theme.text_primary,
                transform
                    * Affine::translate((
                        chip.x0 + 8.0,
                        chip.y0 + if large_text { 10.0 } else { 7.0 },
                    )),
                Some((width - 16.0) as f32),
                TextWeight::Medium,
            );
            self.metadata_filter_cards.push((folder, person, chip));
            filter_x += width + 8.0;
        }
        // On a phone the album-management actions sit beneath the chips, not
        // over the final row of photographs. Reserve their compact strip in
        // the grid geometry so a short library cannot make controls look
        // attached to (or hide) an unrelated picture.
        let compact_album_actions_h =
            if bounds.width() < LIBRARY_TWO_COLUMN_MAX_W && !self.album_editor && !large_text {
                42.0
            } else {
                0.0
            };
        let filter_h = if large_text { 50.0 } else { 34.0 };
        let top = heading_y + header_h + album_h + filter_h + compact_album_actions_h;
        // The worker preserves source aspect ratio. A 3:2 shelf avoids the
        // oversized letterbox trays the old 16:10 geometry produced.
        let preview_h = (card_w * 0.667).clamp(
            76.0,
            if large_text && self.metadata_editor {
                // The sheet owns the right side; retain one complete, bounded
                // background card without making its action row fall below
                // the viewport.
                300.0
            } else if large_text {
                f64::INFINITY
            } else if columns == 1 {
                320.0
            } else {
                178.0
            },
        );
        // The footer has two text lines and an honest 44px action row. Keep
        // it roomy instead of letting desktop density turn controls into
        // loose glyphs or make metadata collide with a filename.
        let card_h = preview_h + if large_text { 128.0 } else { 88.0 };
        let rows = ((bounds.y1 - top - f64::from(PAD)).max(0.0) / (card_h + gap)).floor() as usize;
        // The editor is modal at large text: leaving a partially visible
        // ultra-wide thumbnail behind it creates a false second target and
        // can make its action row look clipped. The local grid decision still
        // drops to one lane, ready for the sheet to close, but the sheet owns
        // the visual surface while it is active.
        let visible = if large_text && self.metadata_editor {
            0
        } else {
            columns.saturating_mul(rows)
        };
        for (index, item) in self.library_items.iter().take(visible).enumerate() {
            let id = &item.id;
            let name = &item.name;
            let favorite = item.favorite;
            let col = index % columns;
            let row = index / columns;
            let x = bounds.x0 + f64::from(PAD) + col as f64 * (card_w + gap);
            let y = top + row as f64 * (card_h + gap);
            let card = Rect::new(x, y, x + card_w, y + card_h);
            let preview = Rect::new(x, y, x + card_w, y + preview_h);
            crate::presentation::paint_card(
                scene,
                transform,
                text_renderer,
                theme,
                card,
                preview,
                self.library_thumbnails.get(&item.thumbnail_key),
                name,
                &item.metadata,
                card.contains(self.pointer),
            );
            let CardActions {
                details,
                remove,
                favorite: star,
            } = card_action_controls(card);
            if !self.album_editor {
                for (target, active) in [
                    (details, details.contains(self.pointer)),
                    (remove, remove.contains(self.pointer)),
                    (star, star.contains(self.pointer)),
                ] {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        if active {
                            theme.bg_hover
                        } else {
                            theme.tokens.colors.neutral[3]
                        },
                        None,
                        &Rect::new(
                            target.x0 + 4.0,
                            target.y0 + 4.0,
                            target.x1 - 4.0,
                            target.y1 - 4.0,
                        ),
                    );
                }
                // Label and pencil centred on the control's centre line
                // (the label used to be top-anchored 14 px down, so its 200%
                // line ran into the face's bottom edge while the glyph sat
                // 8 px higher).
                let (_, details_h) = text_renderer.measure_weighted(
                    "Details",
                    11.0,
                    Some((details.width() - 30.0) as f32),
                    TextWeight::Medium,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    "Details",
                    11.0,
                    theme.tokens.colors.accent[6],
                    transform
                        * Affine::translate((
                            details.x0 + 26.0,
                            details.center().y - f64::from(details_h) / 2.0,
                        )),
                    Some((details.width() - 30.0) as f32),
                    TextWeight::Medium,
                );
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size((details.x0 + 18.0, details.center().y), (16.0, 16.0)),
                    Icon::Pencil,
                    theme.tokens.colors.accent[6],
                    1.5,
                );
                let confirmed = self.library_remove_confirm.as_deref() == Some(id);
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(remove.center(), (16.0, 16.0)),
                    if confirmed { Icon::Check } else { Icon::Trash2 },
                    if confirmed {
                        theme.tokens.colors.semantic.danger
                    } else {
                        theme.text_dim
                    },
                    1.5,
                );
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(star.center(), (17.0, 17.0)),
                    Icon::Star,
                    if favorite {
                        theme.tokens.colors.accent[6]
                    } else {
                        theme.text_dim
                    },
                    1.5,
                );
                self.library_details_cards.push((id.clone(), details));
            }
            if self.album_editor && self.album_editor_members.contains(id) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(card.x0, card.y0, card.x1, card.y0 + 3.0),
                );
            }
            self.library_cards.push((id.clone(), card, favorite));
        }
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
        self.draw_metadata_editor(scene, transform, text_renderer, theme, presence);
    }

    /// Paint the album editor directly over the library, keeping the selected
    /// photographs visible behind it. On narrow screens it becomes a full
    /// width sheet, avoiding a tiny side panel with clipped controls.
    #[allow(
        clippy::too_many_lines,
        reason = "the compact/wide album-sheet geometry and its shared-widget paint path must remain auditable together"
    )]
    fn draw_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.album_editor_hits.clear();
        if !self.library_mode || self.metadata_editor {
            return;
        }
        if self.album_editor && theme.text_scale > 1.25 {
            self.draw_large_text_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        // At enlarged text, album management remains available through the
        // library resource and its semantic actions. Avoid placing fixed-size
        // "New/Edit album" labels into a header whose type has doubled.
        if !self.album_editor && theme.text_scale > 1.25 {
            self.album_editor_rect = Rect::ZERO;
            return;
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 360.0 };
        // A phone must keep actual photographs reachable while editing; a
        // compact bottom sheet leaves the upper grid visible instead of
        // turning “choose photographs” into an instruction with no surface.
        let panel_h = if !self.album_editor && narrow {
            36.0
        } else if narrow {
            258.0
        } else {
            332.0
        };
        let album_chips_h = if self.library_albums.is_empty() {
            0.0
        } else {
            38.0
        };
        let panel_y = if !self.album_editor && narrow {
            // Kept in lock-step with `draw_library`'s album and metadata
            // filter rows before the compact action reserve.
            bounds.y0 + f64::from(PAD) + 31.0 + album_chips_h + 34.0 + 3.0
        } else if narrow {
            bounds.y1 - panel_h - 8.0
        } else {
            bounds.y0 + 10.0
        };
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 12.0
            },
            panel_y,
            if narrow {
                bounds.x1 - 8.0
            } else {
                bounds.x1 - 12.0
            },
            panel_y + panel_h,
        );
        self.album_editor_rect = panel;
        if !self.album_editor {
            let new = Rect::new(panel.x0, panel.y0, panel.x0 + 110.0, panel.y0 + 36.0);
            let edit = Rect::new(new.x1 + 8.0, panel.y0, new.x1 + 116.0, panel.y0 + 36.0);
            for (hit, rect, label, enabled) in [
                (AlbumEditorHit::New, new, "New album", true),
                (
                    AlbumEditorHit::Edit,
                    edit,
                    "Edit album",
                    self.active_album.is_some(),
                ),
            ] {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    if enabled {
                        theme.tokens.colors.surfaces.raised
                    } else {
                        theme.tokens.colors.neutral[2]
                    },
                    None,
                    &rect,
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(rect.x0, rect.y1 - 1.0, rect.x1, rect.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    13.0,
                    if enabled {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    },
                    transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 9.0)),
                    Some(100.0),
                    TextWeight::Medium,
                );
                if enabled {
                    self.album_editor_hits.push((hit, rect));
                }
            }
            return;
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 1.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.album_editor_creating {
                "New album"
            } else {
                "Edit album"
            },
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Album name",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 48.0)),
            Some(180.0),
            TextWeight::Regular,
        );
        let name_rect = Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 66.0,
            panel.x1 - 16.0,
            panel.y0 + 106.0,
        );
        self.album_name_input.apply_layout_external(name_rect);
        self.album_name_input
            .draw(scene, transform, text_renderer, theme, presence);
        text_renderer.draw_text_weighted(
            scene,
            "Choose photographs in the grid",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 122.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            &self.library_page_label(),
            14.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 146.0)),
            Some((panel.width() - 150.0) as f32),
            TextWeight::Medium,
        );
        // Right-aligned to the sheet's 16 px inset — the Next button's edge on
        // the row below — from its measured width, instead of starting at a
        // fixed offset and ending wherever the digits stop.
        let selected = format!("{} selected", self.album_editor_members.len());
        let (selected_w, _) =
            text_renderer.measure_weighted(&selected, 13.0, Some(100.0), TextWeight::Regular);
        text_renderer.draw_text_weighted(
            scene,
            &selected,
            13.0,
            theme.text_dim,
            transform
                * Affine::translate((
                    panel.x1 - 16.0 - f64::from(selected_w.min(100.0)),
                    panel.y0 + 147.0,
                )),
            Some(100.0),
            TextWeight::Regular,
        );
        let page_y = panel.y0 + 166.0;
        let previous = Rect::new(panel.x0 + 16.0, page_y, panel.x0 + 108.0, page_y + 34.0);
        let next = Rect::new(panel.x1 - 108.0, page_y, panel.x1 - 16.0, page_y + 34.0);
        for (hit, rect) in [
            (AlbumEditorHit::PreviousPage, previous),
            (AlbumEditorHit::NextPage, next),
        ] {
            let enabled = match hit {
                AlbumEditorHit::PreviousPage => self.can_page_library(false),
                AlbumEditorHit::NextPage => self.can_page_library(true),
                _ => false,
            };
            if let Some((_, button)) = self
                .album_page_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
        }
        let y = panel.y1 - 48.0;
        let cancel = Rect::new(panel.x0 + 16.0, y, panel.x0 + 88.0, y + 34.0);
        let save = Rect::new(panel.x1 - 92.0, y, panel.x1 - 16.0, y + 34.0);
        let delete = Rect::new(save.x0 - 88.0, y, save.x0 - 8.0, y + 34.0);
        for (hit, rect, label) in [
            (AlbumEditorHit::Cancel, cancel, "Cancel"),
            (AlbumEditorHit::Save, save, "Save"),
        ] {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[3],
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                13.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 9.0)),
                Some((rect.width() - 16.0) as f32),
                TextWeight::Medium,
            );
            let focused = matches!(hit, AlbumEditorHit::Cancel) && self.album_editor_focus == 1
                || matches!(hit, AlbumEditorHit::Save)
                    && self.album_editor_focus == if self.album_editor_creating { 2 } else { 3 };
            if focused {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + 2.0),
                );
            }
            self.album_editor_hits.push((hit, rect));
        }
        if !self.album_editor_creating {
            let (hit, label) = if self.album_delete_confirm {
                (AlbumEditorHit::ConfirmDelete, "Delete album?")
            } else {
                (AlbumEditorHit::Delete, "Delete")
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.semantic.danger,
                None,
                &delete,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                theme.text_primary,
                transform * Affine::translate((delete.x0 + 7.0, delete.y0 + 9.0)),
                Some((delete.width() - 10.0) as f32),
                TextWeight::Medium,
            );
            if self.album_editor_focus == 2 {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(delete.x0, delete.y0, delete.x1, delete.y0 + 2.0),
                );
            }
            self.album_editor_hits.push((hit, delete));
        }
    }

    /// A large-text album sheet uses a short vertical rhythm instead of trying
    /// to scale the desktop side panel into a phone viewport.  Its action
    /// targets are still the same local intents and shared `TextInput` as the
    /// regular sheet; only this device's presentation changes.
    #[allow(
        clippy::too_many_lines,
        reason = "large-text album geometry deliberately keeps every field and action in one auditable sheet"
    )]
    fn draw_large_text_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 460.0 };
        let panel_h = (bounds.height() - 16.0).min(620.0);
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 12.0
            },
            if narrow {
                bounds.y1 - panel_h - 8.0
            } else {
                bounds.y0 + 12.0
            },
            if narrow {
                bounds.x1 - 8.0
            } else {
                bounds.x1 - 12.0
            },
            if narrow {
                bounds.y1 - 8.0
            } else {
                bounds.y0 + 12.0 + panel_h
            },
        );
        self.album_editor_rect = panel;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 1.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.album_editor_creating {
                "New album"
            } else {
                "Edit album"
            },
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Album name",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 66.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        self.album_name_input.apply_layout_external(Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 94.0,
            panel.x1 - 16.0,
            panel.y0 + 170.0,
        ));
        self.album_name_input
            .draw(scene, transform, text_renderer, theme, presence);
        text_renderer.draw_text_weighted(
            scene,
            "Choose photographs in the grid",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 196.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!("{} selected", self.album_editor_members.len()),
            12.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 248.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        let page_y = panel.y0 + 290.0;
        let previous = Rect::new(panel.x0 + 16.0, page_y, panel.x0 + 166.0, page_y + 56.0);
        let next = Rect::new(panel.x1 - 166.0, page_y, panel.x1 - 16.0, page_y + 56.0);
        for (hit, rect, label) in [
            (AlbumEditorHit::PreviousPage, previous, "Previous"),
            (AlbumEditorHit::NextPage, next, "Next"),
        ] {
            let enabled = match hit {
                AlbumEditorHit::PreviousPage => self.can_page_library(false),
                AlbumEditorHit::NextPage => self.can_page_library(true),
                _ => false,
            };
            scene.fill(
                Fill::NonZero,
                transform,
                if enabled {
                    theme.tokens.colors.neutral[3]
                } else {
                    theme.tokens.colors.neutral[2]
                },
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                if enabled {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 16.0)),
                Some((rect.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            if enabled {
                self.album_editor_hits.push((hit, rect));
            }
        }
        let action_h = 60.0;
        let action_y = panel.y1 - action_h - 16.0;
        let cancel = Rect::new(
            panel.x0 + 16.0,
            action_y,
            panel.x0 + 166.0,
            action_y + action_h,
        );
        let save = Rect::new(
            panel.x1 - 166.0,
            action_y,
            panel.x1 - 16.0,
            action_y + action_h,
        );
        let delete = Rect::new(
            panel.x0 + 16.0,
            action_y - action_h - 10.0,
            panel.x1 - 16.0,
            action_y - 10.0,
        );
        for (hit, rect, label, color) in [
            (
                AlbumEditorHit::Cancel,
                cancel,
                "Cancel",
                theme.tokens.colors.neutral[3],
            ),
            (
                AlbumEditorHit::Save,
                save,
                "Save",
                theme.tokens.colors.accent[6],
            ),
        ] {
            scene.fill(Fill::NonZero, transform, color, None, &rect);
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 17.0)),
                Some((rect.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            self.album_editor_hits.push((hit, rect));
        }
        if !self.album_editor_creating {
            let hit = if self.album_delete_confirm {
                AlbumEditorHit::ConfirmDelete
            } else {
                AlbumEditorHit::Delete
            };
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.semantic.danger,
                None,
                &delete,
            );
            text_renderer.draw_text_weighted(
                scene,
                if self.album_delete_confirm {
                    "Delete album?"
                } else {
                    "Delete album"
                },
                12.0,
                theme.text_primary,
                transform * Affine::translate((delete.x0 + 12.0, delete.y0 + 17.0)),
                Some((delete.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            self.album_editor_hits.push((hit, delete));
        }
    }

    /// A focused details sheet built from shared `TextInputs`. It intentionally
    /// asks for labels rather than offering facial analysis: people metadata is
    /// explicit, editable, and replicated only after Save.
    #[allow(
        clippy::too_many_lines,
        reason = "the metadata sheet paints four shared fields and its actions in one auditable geometry block"
    )]
    fn draw_metadata_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.metadata_editor_hits.clear();
        if !self.metadata_editor {
            return;
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 420.0 };
        let scale = f64::from(theme.text_scale);
        let panel_x0 = if narrow {
            bounds.x0 + 8.0
        } else {
            bounds.x1 - panel_w - 12.0
        };
        let panel_x1 = if narrow {
            bounds.x1 - 8.0
        } else {
            bounds.x1 - 12.0
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "sheet widths are ordinary logical-pixel values"
        )]
        let inner_w = (panel_x1 - panel_x0 - 2.0 * SHEET_INSET).max(0.0) as f32;

        // Measured header and field rhythm (see the `SHEET_*` constants).
        let (_, title_h) =
            text_renderer.measure_weighted(SHEET_TITLE, 18.0, Some(inner_w), TextWeight::Medium);
        let (_, subtitle_h) = text_renderer.measure_weighted(
            SHEET_SUBTITLE,
            12.0,
            Some(inner_w),
            TextWeight::Regular,
        );
        let label_h = SHEET_FIELD_LABELS
            .iter()
            .map(|label| {
                f64::from(
                    text_renderer
                        .measure_weighted(label, 12.0, Some(inner_w), TextWeight::Regular)
                        .1,
                )
            })
            .fold(0.0_f64, f64::max);
        let title_top = SHEET_TOP_PAD * scale;
        let subtitle_top = title_top + f64::from(title_h) + SHEET_TITLE_GAP;
        let fields_top = subtitle_top + f64::from(subtitle_h) + SHEET_SUBTITLE_GAP;
        let input_h = SHEET_INPUT_H * scale;
        let field_gap = SHEET_FIELD_GAP * scale;
        let field_h = label_h + SHEET_LABEL_GAP * scale + input_h;
        let field_pitch = field_h + field_gap;
        let content_h = fields_top + 4.0 * field_pitch - field_gap + SHEET_CONTENT_PAD_BOTTOM;
        let footer_h = SHEET_FOOTER_H * scale;
        let wanted_h = content_h + SHEET_BODY_FOOTER_GAP + footer_h;
        let panel_h = wanted_h.min((bounds.height() - 24.0).max(0.0));
        let panel = Rect::new(
            panel_x0,
            if narrow {
                (bounds.y1 - panel_h - 8.0).max(bounds.y0 + 8.0)
            } else {
                bounds.y0 + 12.0
            },
            panel_x1,
            if narrow {
                bounds.y1 - 8.0
            } else {
                (bounds.y0 + 12.0 + panel_h).min(bounds.y1 - 8.0)
            },
        );
        // The footer is pinned; the header and fields scroll in the body
        // above it when the capped sheet cannot hold them all.
        let footer_top = panel.y1 - footer_h;
        let body = Rect::new(
            panel.x0,
            panel.y0,
            panel.x1,
            (footer_top - SHEET_BODY_FOOTER_GAP).max(panel.y0),
        );
        let max_scroll = (content_h - body.height()).max(0.0);
        if std::mem::take(&mut self.metadata_editor_reveal_focus) && self.metadata_editor_focus < 4
        {
            let top = fields_top + self.metadata_editor_focus as f64 * field_pitch;
            let bottom = top + field_h;
            if top - self.metadata_editor_scroll < 0.0 {
                self.metadata_editor_scroll = top - SHEET_INSET;
            } else if bottom - self.metadata_editor_scroll > body.height() {
                self.metadata_editor_scroll = bottom - body.height() + SHEET_INSET;
            }
        }
        self.metadata_editor_scroll = self.metadata_editor_scroll.clamp(0.0, max_scroll);
        self.metadata_editor_body = body;
        let origin_y = panel.y0 - self.metadata_editor_scroll;

        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        text_renderer.draw_text_weighted(
            scene,
            SHEET_TITLE,
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + SHEET_INSET, origin_y + title_top)),
            Some(inner_w),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            SHEET_SUBTITLE,
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + SHEET_INSET, origin_y + subtitle_top)),
            Some(inner_w),
            TextWeight::Regular,
        );
        let fields = [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
        ];
        for (index, (label, input)) in SHEET_FIELD_LABELS.iter().zip(fields).enumerate() {
            let y = origin_y + fields_top + index as f64 * field_pitch;
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                theme.text_dim,
                transform * Affine::translate((panel.x0 + SHEET_INSET, y)),
                Some(inner_w),
                TextWeight::Regular,
            );
            let input_top = y + label_h + SHEET_LABEL_GAP * scale;
            input.apply_layout_external(Rect::new(
                panel.x0 + SHEET_INSET,
                input_top,
                panel.x1 - SHEET_INSET,
                input_top + input_h,
            ));
            input.draw(scene, transform, text_renderer, theme, presence);
        }
        scene.pop_layer();
        // Top hairline over the (possibly scrolled) content.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 1.0),
        );
        let y = footer_top;
        let button_h = SHEET_BUTTON_H * scale;
        let cancel = Rect::new(
            panel.x0 + SHEET_INSET,
            y,
            panel.x0 + 112.0 * scale,
            y + button_h,
        );
        let save = Rect::new(
            panel.x1 - 96.0 * scale,
            y,
            panel.x1 - SHEET_INSET,
            y + button_h,
        );
        for (hit, rect, label, color) in [
            (
                MetadataEditorHit::Cancel,
                cancel,
                "Cancel",
                theme.tokens.colors.neutral[3],
            ),
            (
                MetadataEditorHit::Save,
                save,
                "Save",
                theme.tokens.colors.accent[6],
            ),
        ] {
            let focused = matches!(hit, MetadataEditorHit::Cancel)
                && self.metadata_editor_focus == 4
                || matches!(hit, MetadataEditorHit::Save) && self.metadata_editor_focus == 5;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                if focused { theme.bg_hover } else { color },
                None,
                &rect,
            );
            // Label centred on its measured line (the old `14 × scale` top
            // offset only matched the line at 1×).
            let (_, label_line) = text_renderer.measure_weighted(
                label,
                13.0,
                Some((rect.width() - 16.0) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                13.0,
                theme.text_primary,
                transform
                    * Affine::translate((
                        rect.x0 + 10.0,
                        rect.center().y - f64::from(label_line) / 2.0,
                    )),
                Some((rect.width() - 16.0) as f32),
                TextWeight::Medium,
            );
            self.metadata_editor_hits.push((hit, rect));
        }
    }
}

/// The region of a `w`×`h` image `viewer` currently frames, in normalized
/// `0..1` coordinates — the inverse of the transform `ImageViewer::draw`
/// applies (contain-fit, then the widget's own zoom and pan). `None` when the
/// viewer or the image has a zero dimension.
fn visible_image_rect(viewer: &ImageViewer, w: f64, h: f64) -> Option<Rect01> {
    let rect = Widget::get_rect(viewer);
    let (rw, rh) = (rect.width(), rect.height());
    if rw <= 0.0 || rh <= 0.0 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let base = (rw / w).min(rh / h);
    let total_scale = base * f64::from(viewer.zoom());
    if total_scale <= 0.0 {
        return None;
    }
    let pan = viewer.pan();
    let (cx, cy) = ((rect.x0 + rect.x1) * 0.5, (rect.y0 + rect.y1) * 0.5);
    let (dw, dh) = (w * total_scale, h * total_scale);
    let dx = cx - dw * 0.5 + pan.x * total_scale;
    let dy = cy - dh * 0.5 + pan.y * total_scale;

    let x0 = ((rect.x0 - dx) / total_scale / w).clamp(0.0, 1.0);
    let x1 = ((rect.x1 - dx) / total_scale / w).clamp(0.0, 1.0);
    let y0 = ((rect.y0 - dy) / total_scale / h).clamp(0.0, 1.0);
    let y1 = ((rect.y1 - dy) / total_scale / h).clamp(0.0, 1.0);
    Some(Rect01 {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0.0),
        h: (y1 - y0).max(0.0),
    })
}

/// Keep compact card chrome single-line.  Character iteration, rather than a
/// byte slice, preserves Unicode names in both painting and accessibility.
fn compact_label(label: &str, max_chars: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Build one toolbar button in the presentation `face` asks for.
///
/// A glyph-only button keeps its words as the tooltip, which
/// [`ui::widget::Button`] also adopts as its accessible name — so dropping the
/// label costs the pointer user a hover and the screen-reader user nothing.
fn build_button(
    hit: Hit,
    key: &str,
    icon: Icon,
    face: Face,
    btn_h: f32,
    pending: &Rc<Cell<Option<Hit>>>,
) -> Button {
    let label = tr(key);
    let slot = pending.clone();
    let shown = match face {
        Face::Labelled => label.as_str(),
        Face::IconOnly | Face::LargeOverflow => "",
    };
    // “Library” has a measured 104 px compact face (including icon and
    // padding); the conservative generic estimate is 120 px. Keeping the
    // compact value prevents the first phone button from being pushed a few
    // pixels beyond the left edge while retaining room for every glyph.
    let width = if key == "photos-library" && matches!(face, Face::Labelled) {
        104.0
    } else {
        item_width(face, &label, btn_h)
    };
    Button::new(key, shown, None)
        .with_icon(icon)
        .with_size(width, btn_h)
        .tooltip(label.clone())
        .with_accessible_name(&label)
        .on_effect(move || slot.set(Some(hit)))
}

/// A local navigation button in the album sheet. Membership paging is device
/// presentation state, so its callback only posts local intent; it never
/// mutates the document from inside the widget.
fn build_album_page_button(
    hit: AlbumEditorHit,
    id: &str,
    label: &str,
    pending: &Rc<Cell<Option<AlbumEditorHit>>>,
) -> Button {
    let slot = pending.clone();
    Button::new(id, label, None)
        .with_size(92.0, 34.0)
        .tooltip(label)
        .with_accessible_name(label)
        .on_effect(move || slot.set(Some(hit)))
}

fn build_album_page_buttons(
    pending: &Rc<Cell<Option<AlbumEditorHit>>>,
) -> Vec<(AlbumEditorHit, Button)> {
    [
        (
            AlbumEditorHit::PreviousPage,
            "photos-album-previous-page",
            "Previous",
        ),
        (AlbumEditorHit::NextPage, "photos-album-next-page", "Next"),
    ]
    .into_iter()
    .map(|(hit, id, label)| (hit, build_album_page_button(hit, id, label, pending)))
    .collect()
}

/// Wrap an RGBA buffer as the image type the scene draws.
fn to_peniko(img: &Rgba) -> ImageData {
    pixels::to_peniko(img)
}

/// A local decode-cache key. The content hash, when present, is immutable;
/// include the legacy path too so older documents remain distinguishable.
fn decode_key(open: &OpenImage) -> String {
    format!(
        "{}\0{}",
        open.path,
        open.content.as_deref().unwrap_or_default()
    )
}

fn thumbnail_key(open: &OpenImage) -> String {
    format!("{}:320x180", decode_key(open))
}

impl PhotosApp {
    fn reconcile_content(&mut self, doc: &AutoCommit) {
        let current = LiveContent::read(doc);
        if self
            .import_content
            .as_ref()
            .is_some_and(|old| old != &current)
        {
            self.picker_stale = true;
            if let Some((invocation, _)) = self.pending_resource_open.take() {
                self.resource_result = Some(qos_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("Photo import invalidated by changed document content".into()),
                });
                self.import_content = None;
            }
        }
        if let Some(old) = &self.observed_content
            && old != &current
        {
            self.pending.take();
            self.album_page_pending.take();
            self.compact_more_open = false;
            self.library_cards.clear();
            self.library_details_cards.clear();
            let metadata_changed = self
                .metadata_editor_id
                .as_ref()
                .is_some_and(|id| old.library.photo(id) != current.library.photo(id));
            let album_changed = self
                .album_editor_id
                .as_ref()
                .is_some_and(|id| old.library.album(id) != current.library.album(id))
                || self
                    .album_editor_members
                    .iter()
                    .any(|id| current.library.photo(id).is_none());
            if metadata_changed {
                self.cancel_metadata_editor();
                self.notice = Some("Photo changed; unsaved details discarded".into());
            }
            if album_changed {
                self.cancel_album_editor();
                self.notice =
                    Some("Album or membership changed; unsaved album changes discarded".into());
            }
            if self
                .active_album
                .as_ref()
                .is_some_and(|id| current.library.album(id).is_none())
            {
                self.active_album = None;
            }
        }
        self.observed_content = Some(current);
        // Recreating the viewer on changed immutable identity/edits also drops
        // an active drag before the next input can apply its stale viewport.
        self.refresh(doc);
    }

    /// Take completed thumbnail work without blocking the UI thread.
    fn drain_thumbnail_results(&mut self) {
        while let Ok(result) = self.thumbnail_rx.try_recv() {
            self.accept_thumbnail_result(result);
        }
    }

    /// Accept a worker result only when it still belongs to the materialised
    /// library slice. This is the generation fence that makes scrolling,
    /// filtering and collaboration safe while a decode is in flight.
    fn accept_thumbnail_result(&mut self, result: ThumbnailResult) {
        if Some(result.generation) != self.thumbnail_generation
            || !self.thumbnail_visible_ids.contains(&result.id)
        {
            return;
        }
        self.thumbnail_queued.remove(&result.id);
        if let Some(preview) = result.preview {
            self.library_thumbnails
                .insert(result.id, to_peniko(&preview));
        } else {
            self.thumbnail_unavailable.insert(result.id);
        }
    }

    /// Keep thumbnail work to one small queued item per synchronization. The
    /// worker, rather than this method, may read a file or decode bytes.
    fn schedule_library_thumbnail(&mut self, sources: Vec<LibraryPhoto>, document_changed: bool) {
        let ids: Vec<_> = sources
            .iter()
            .map(|photo| thumbnail_key(&photo.image))
            .collect();
        if document_changed || ids != self.thumbnail_visible_ids {
            let Some(next_generation) = self
                .thumbnail_generation
                .and_then(|generation| generation.checked_add(1))
            else {
                // Never reuse a generation token. Any result still in the
                // worker's queue is now untrusted, and no new work is sent.
                self.thumbnail_generation = None;
                self.thumbnail_visible_ids = ids;
                self.thumbnail_queued.clear();
                self.library_thumbnails.clear();
                self.thumbnail_unavailable.clear();
                return;
            };
            self.thumbnail_generation = Some(next_generation);
            self.thumbnail_visible_ids = ids;
            self.thumbnail_queued.clear();
            if document_changed {
                self.library_thumbnails.clear();
            }
            self.library_thumbnails
                .retain(|id, _| self.thumbnail_visible_ids.contains(id));
            self.thumbnail_unavailable
                .retain(|id| self.thumbnail_visible_ids.contains(id));
        }
        let (Some(tx), Some(generation)) = (self.thumbnail_tx.as_ref(), self.thumbnail_generation)
        else {
            return;
        };
        let Some(photo) = sources.into_iter().find(|photo| {
            !self
                .library_thumbnails
                .contains_key(&thumbnail_key(&photo.image))
                && !self
                    .thumbnail_unavailable
                    .contains(&thumbnail_key(&photo.image))
                && !self.thumbnail_queued.contains(&thumbnail_key(&photo.image))
        }) else {
            return;
        };
        let id = thumbnail_key(&photo.image);
        let job = ThumbnailJob {
            generation,
            id: id.clone(),
            image: photo.image,
        };
        if tx.try_send(job).is_ok() {
            self.thumbnail_queued.insert(id);
        }
    }
}

impl Widget for PhotosApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::PHOTOS_OPERATION_TYPE]
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            crate::actions::PhotosOperation::ViewAlbum { album_id } => {
                self.view_album(doc, &album_id)?;
            }
            crate::actions::PhotosOperation::ViewFolder { folder } => {
                self.view_metadata_filter(doc, Some(folder), None, None)?;
            }
            crate::actions::PhotosOperation::ViewPerson { person } => {
                self.view_metadata_filter(doc, None, Some(person), None)?;
            }
            crate::actions::PhotosOperation::ViewTag { tag } => {
                self.view_metadata_filter(doc, None, None, Some(tag))?;
            }
            crate::actions::PhotosOperation::Open { path: Some(_) } => {
                return Err("direct open requires lifecycle dispatch".into());
            }
            crate::actions::PhotosOperation::Save { path } => {
                let open =
                    PhotosDoc::image(doc).ok_or_else(|| "no photograph is open".to_string())?;
                let destination = path.unwrap_or_else(|| Self::suggested_save_path(&open));
                if !self.save_to(&destination) {
                    return Err(tr("photos-save-failed"));
                }
            }
            other => self.perform_effect(other, doc)?,
        }
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if let crate::actions::PhotosOperation::ExportEditedContent {
            resource_id,
            source_content,
            edits,
        } = &operation
        {
            let output =
                crate::actions::export_edited_content(resource_id, source_content, *edits, doc)?;
            return Ok(Some(ui::widget::ResourceOperationStart::Completed(output)));
        }
        if let crate::actions::PhotosOperation::Open { path: Some(file) } = operation {
            self.import_content = Some(LiveContent::read(doc));
            self.pending_resource_open = Some((invocation, file));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if self.picker.is_some() {
            self.resource_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                qos_actions::ActionOutput::default(),
            )))
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "synchronization derives the bounded library, filters, search, and thumbnail work from one document snapshot"
    )]
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        self.reconcile_content(doc);
        if let Some((invocation, path)) = self.pending_resource_open.take() {
            self.import_content = None;
            self.open_path(doc, &path);
            self.resource_result = Some(if self.notice.is_none() {
                qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                }
            } else {
                qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "photos.open_failed".into(),
                        message: tr("photos-open-failed"),
                        details: None,
                        retryable: false,
                    },
                }
            });
        }
        self.drain_picker(doc);
        self.refresh(doc);

        // An address can arrive before its bytes. Do not offer Copy, Save or
        // a blind crop against an empty canvas; the durable resource remains
        // available to a headless collaborator that deliberately chooses it.
        let has_image = self.has_image();
        let edited = !PhotosDoc::edits(doc).is_identity();
        for (hit, button) in &mut self.buttons {
            button.set_enabled(Self::enabled(*hit, has_image, edited));
        }
        self.status = self.status_text(doc);
        let heads = doc.get_heads();
        let document_changed = heads != self.search_heads;
        if document_changed {
            self.search_provider = crate::search::PhotosSearchProvider::new(doc);
            self.search_heads = heads;
            self.thumbnail_unavailable.clear();
        }
        let library = PhotosDoc::library(doc);
        let membership = self.active_album.as_ref().and_then(|id| {
            library
                .albums
                .iter()
                .find(|album| &album.id == id)
                .map(|album| &album.photo_ids)
        });
        let matches_filter = |photo: &LibraryPhoto| {
            membership.is_none_or(|ids| ids.contains(&photo.id))
                && self
                    .active_folder
                    .as_ref()
                    .is_none_or(|folder| photo.folder.as_ref() == Some(folder))
                && self
                    .active_person
                    .as_ref()
                    .is_none_or(|person| photo.people.contains(person))
                && self
                    .active_tag
                    .as_ref()
                    .is_none_or(|tag| photo.tags.contains(tag))
        };
        self.library_total = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .count();
        self.library_offset = self
            .library_offset
            .min(self.library_total.saturating_sub(1));
        self.library_items = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .skip(self.library_offset)
            .take(120)
            .map(|photo| LibraryItem {
                id: photo.id.clone(),
                thumbnail_key: thumbnail_key(&photo.image),
                name: photo.image.name.clone(),
                favorite: photo.favorite,
                metadata: photo.metadata_summary(),
            })
            .collect();
        self.library_albums = library
            .albums
            .iter()
            .take(24)
            .map(|album| (album.id.clone(), album.name.clone(), album.photo_ids.len()))
            .collect();
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        for photo in &library.photos {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in &photo.people {
                *people.entry(person.clone()).or_default() += 1;
            }
        }
        self.library_folders = folders.into_iter().take(6).collect();
        self.library_people = people.into_iter().take(6).collect();
        let thumbnail_sources: Vec<_> = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .skip(self.library_offset)
            .take(24)
            .cloned()
            .collect();
        self.drain_thumbnail_results();
        self.schedule_library_thumbnail(thumbnail_sources, document_changed);
    }

    #[allow(
        clippy::unwrap_used,
        reason = "root's taffy node always builds from freshly-built children"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Refit against the width the last layout produced. The first frame
        // has no rect yet and lays out roomy; the frame after corrects it.
        // Large text uses the same labelled More overflow model as a phone:
        // a small, visible “More” doorway keeps the hidden commands
        // discoverable, while the four immediate navigation/edit controls
        // retain their accessible icon names without colliding.
        self.large_text_toolbar = theme.text_scale > 1.25;
        self.refit_toolbar(self.rect.width() as f32, btn_h(theme.text_scale));

        let bar = self.build_toolbar_node(taffy, theme, node_map);
        let canvas = self.build_canvas_node(taffy, theme, node_map);
        let status = self.build_status_node(taffy, theme.text_scale);

        let root = taffy
            .new_with_children(
                Style {
                    flex_direction: FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[bar, canvas, status],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        let here = Point::new(x, y);

        let children = taffy.children(node).unwrap();
        let Some(&bar) = children.first() else { return };
        let bl = taffy.layout(bar).unwrap();
        self.toolbar_rect = Rect::new(
            x + f64::from(bl.location.x),
            y + f64::from(bl.location.y),
            x + f64::from(bl.location.x) + f64::from(bl.size.width),
            y + f64::from(bl.location.y) + f64::from(bl.size.height),
        );
        let bar_origin = Point::new(x + f64::from(bl.location.x), y + f64::from(bl.location.y));
        let bar_children = taffy.children(bar).unwrap();
        self.separator_rects.clear();
        for (slot, &bn) in self.toolbar_slots().iter().zip(bar_children.iter()) {
            match *slot {
                Slot::Item(n) => {
                    if let Some((_, button)) = self.buttons.get_mut(n) {
                        button.apply_layout(taffy, bn, bar_origin);
                    }
                }
                Slot::Separator => {
                    let sl = taffy.layout(bn).unwrap();
                    let sx =
                        bar_origin.x + f64::from(sl.location.x) + f64::from(sl.size.width) / 2.0;
                    let sy = bar_origin.y + f64::from(sl.location.y);
                    self.separator_rects.push(Rect::new(
                        sx,
                        sy,
                        sx + 1.0,
                        sy + f64::from(sl.size.height),
                    ));
                }
                Slot::Spacer => {}
            }
        }

        if let Some(&canvas) = children.get(1) {
            let cl = taffy.layout(canvas).unwrap();
            self.canvas_rect = Rect::new(
                x + f64::from(cl.location.x),
                y + f64::from(cl.location.y),
                x + f64::from(cl.location.x) + f64::from(cl.size.width),
                y + f64::from(cl.location.y) + f64::from(cl.size.height),
            );
            if let Some(v) = self.viewer.as_mut() {
                v.apply_layout(taffy, canvas, here);
            }
        }
        if let Some(&status) = children.get(2) {
            let sl = taffy.layout(status).unwrap();
            self.status_rect = Rect::new(
                x + f64::from(sl.location.x),
                y + f64::from(sl.location.y),
                x + f64::from(sl.location.x) + f64::from(sl.size.width),
                y + f64::from(sl.location.y) + f64::from(sl.size.height),
            );
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        // App background. Every colour on this path comes from the theme.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        // The toolbar sits on the raised surface so it reads as chrome
        // against the canvas below it.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.toolbar_rect,
        );
        // …and is separated from the canvas by a real rule, not only by that
        // fill. Under the high-contrast palette `surfaces.raised` and
        // `surfaces.base` are both pure black by design — that theme
        // deliberately carries structure in borders rather than in fills — so
        // a toolbar defined only by its plate has no edge at all there. The
        // same rule above the status line, for the same reason.
        // The status line's rule is drawn only when the line has something in
        // it; an empty band would otherwise be fenced off as if it did.
        let mut edges = vec![self.toolbar_rect.y1];
        if !self.status.is_empty() {
            edges.push(self.status_rect.y0);
        }
        for edge in edges {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(self.rect.x0, edge - 1.0, self.rect.x1, edge),
            );
        }

        // Group rules: what opens a file, what adjusts it, what exports it.
        for sep in &self.separator_rects {
            scene.fill(Fill::NonZero, transform, theme.border_standard, None, sep);
        }

        for slot in self.toolbar_slots() {
            let Slot::Item(index) = *slot else { continue };
            if let Some((_, button)) = self.buttons.get_mut(index) {
                button.draw(scene, transform, text_renderer, theme, presence);
            }
        }
        // Button chrome has a fixed 34px height.  Paint the one high-text
        // overflow word ourselves at a smaller, centred size so it remains
        // visibly labelled without letting a 200%-scaled button caption
        // bleed through the toolbar rule.
        if self.large_text_toolbar
            && let Some((_, more)) = self.buttons.get(9)
        {
            let rect = more.get_rect();
            text_renderer.draw_text_weighted(
                scene,
                "More",
                10.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 34.0, rect.y0 + 8.0)),
                Some((rect.width() - 40.0) as f32),
                TextWeight::Medium,
            );
        }
        if self.library_mode {
            self.draw_library(scene, transform, text_renderer, theme, presence);
        } else if let Some(v) = self.viewer.as_mut() {
            v.draw(scene, transform, text_renderer, theme, presence);
        } else {
            let (title, body) = self.empty_state_text();
            EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.canvas_rect,
                Some(Icon::Image),
                &title,
                Some(&body),
                None,
                Point::ZERO,
            );
        }
        // This is deliberately after the canvas/library so the compact menu
        // is a real popup rather than a panel hidden behind the grid.
        self.draw_compact_more(scene, transform, text_renderer, theme);

        if !self.status.is_empty() {
            let inner = self.status_rect;
            // Centre the line on its measured height inside a band sized by
            // `status_h` — at 2× the filename used to run off the bottom of a
            // fixed band anchored 4 px from its top (plan task 19).
            let budget = (inner.width() - 2.0 * f64::from(PAD)) as f32;
            let (_, line_h) = text_renderer.measure_weighted(
                &self.status,
                STATUS_TEXT_SIZE,
                Some(budget),
                TextWeight::Regular,
            );
            let y = inner.y0 + (inner.height() - f64::from(line_h)).max(0.0) / 2.0;
            text_renderer.draw_text_weighted(
                scene,
                &self.status,
                STATUS_TEXT_SIZE,
                theme.text_primary,
                transform * Affine::translate((inner.x0 + f64::from(PAD), y)),
                Some(budget),
                TextWeight::Regular,
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event entry point preserves explicit precedence between album chips, grid cards, toolbar, and viewer"
    )]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.reconcile_content(doc);
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // The phone overflow is a real keyboard menu, not a pointer-only
        // drawing. Its entries are normal canonical toolbar actions, so this
        // just supplies focus traversal and activation for the compact locus.
        if self.compact_more_open
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.compact_more_open = false;
                }
                Key::Named(NamedKey::ArrowDown) => self.step_compact_more_focus(true),
                Key::Named(NamedKey::ArrowUp) => self.step_compact_more_focus(false),
                Key::Named(NamedKey::Enter) => {
                    if let Some(hit) = self.focused_compact_more_hit()
                        && self.is_toolbar_hit_enabled(hit)
                    {
                        self.pending.set(Some(hit));
                        self.compact_more_open = false;
                        self.drain_pending(doc);
                    }
                }
                Key::Character(text) if text == " " => {
                    if let Some(hit) = self.focused_compact_more_hit()
                        && self.is_toolbar_hit_enabled(hit)
                    {
                        self.pending.set(Some(hit));
                        self.compact_more_open = false;
                        self.drain_pending(doc);
                    }
                }
                _ => return,
            }
            *handled = true;
            return;
        }

        // The labeled compact overflow is painted above both the viewer and
        // the library.  Give it first refusal of clicks so an action in the
        // popup cannot also select a photograph behind it.
        if self.compact_more_open
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .compact_more_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.pending.set(Some(hit));
            self.compact_more_open = false;
            self.drain_pending(doc);
            *handled = true;
            return;
        }

        if self.library_mode
            && self.album_editor
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let direction = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => y.signum(),
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y.signum() as f32,
            };
            if direction < 0.0 {
                self.page_library(true);
            } else if direction > 0.0 {
                self.page_library(false);
            }
            *handled = true;
            return;
        }

        if self.library_mode
            && self.handle_metadata_editor_event(
                event,
                scale_factor,
                *modifiers,
                doc,
                path,
                handled,
                node_id,
                taffy,
            )
        {
            return;
        }

        if self.library_mode
            && self.handle_album_editor_event(
                event,
                scale_factor,
                *modifiers,
                doc,
                path,
                handled,
                node_id,
                taffy,
            )
        {
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            match hit {
                AlbumEditorHit::New => self.begin_new_album(doc),
                AlbumEditorHit::Edit => self.begin_edit_album(doc),
                _ => {}
            }
            *handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .album_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = album_id;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.library_offset = 0;
            *handled = true;
            return;
        }
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((folder, person, _)) = self
                .metadata_filter_cards
                .iter()
                .find(|(_, _, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = None;
            self.active_folder = folder;
            self.active_person = person;
            self.active_tag = None;
            self.library_offset = 0;
            self.notice = None;
            *handled = true;
            return;
        }
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((id, _)) = self
                .library_details_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.begin_metadata_editor(doc, &id);
            *handled = true;
            return;
        }
        if self.library_mode
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let direction = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => y.signum(),
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y.signum() as f32,
            };
            if direction < 0.0 {
                self.page_library(true);
            } else if direction > 0.0 {
                self.page_library(false);
            }
            *handled = true;
            return;
        }

        // A library card is a direct manipulation surface, but it is still
        // only a front end for a canonical action. The trailing star and
        // remove affordance have their own hit areas, so destructive and
        // metadata changes do not unexpectedly open the editor.
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((photo_id, card, favorite)) = self
                .library_cards
                .iter()
                .find(|(_, card, _)| card.contains(self.pointer))
                .cloned()
        {
            // Hit the same rects `draw_library` painted (`card_action_controls`).
            let actions = card_action_controls(card);
            if !self.album_editor
                && (actions.remove.contains(self.pointer)
                    || actions.favorite.contains(self.pointer))
            {
                if actions.favorite.contains(self.pointer) {
                    self.library_remove_confirm = None;
                    self.commit_library_action(
                        doc,
                        &crate::actions::PhotosAction::SetFavorite {
                            photo_id,
                            favorite: !favorite,
                        },
                    );
                } else if self.library_remove_confirm.as_deref() == Some(&photo_id) {
                    self.commit_library_action(
                        doc,
                        &crate::actions::PhotosAction::RemovePhoto { photo_id },
                    );
                    self.library_remove_confirm = None;
                    self.notice = Some("Photograph removed from the shared library".to_owned());
                } else {
                    self.library_remove_confirm = Some(photo_id);
                    self.notice = Some(
                        "Click × again to remove it from the library and every album".to_owned(),
                    );
                }
            } else {
                self.library_remove_confirm = None;
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::SelectPhoto { photo_id },
                );
                self.library_mode = false;
            }
            *handled = true;
            return;
        }
        let children = taffy.children(node_id).unwrap_or_default();
        if let Some(&bar) = children.first() {
            let bar_children = taffy.children(bar).unwrap_or_default();
            for (slot, &bn) in self.toolbar_slots().iter().zip(bar_children.iter()) {
                let Slot::Item(n) = *slot else { continue };
                let Some((_, button)) = self.buttons.get_mut(n) else {
                    continue;
                };
                button.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    doc,
                    path,
                    handled,
                    focus_target,
                    bn,
                    taffy,
                );
            }
        }
        if let (Some(v), Some(&canvas)) = (self.viewer.as_mut(), children.get(1)) {
            v.handle_event(
                event,
                scale_factor,
                modifiers,
                doc,
                path,
                handled,
                focus_target,
                canvas,
                taffy,
            );
        }
        self.drain_pending(doc);
    }

    fn name(&self) -> &'static str {
        "PhotosApp"
    }

    /// Hand the concrete app back out of a `Box<dyn Widget>`.
    ///
    /// A compositor hosting this in a tab holds it as a trait object, so
    /// "open this file in the photo viewer that is already on screen" has no
    /// route in without it. Opening still goes through
    /// [`PhotosApp::open_path`] — this only recovers the type, it does not add
    /// a second way to change what is shown.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.accessibility_tree())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        self.reconcile_content(state);
        self.perform_toolbar_semantic_action(target, action, state)
            || self.perform_library_semantic_action(target, action, state)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0, button] => self
                .buttons
                .get(*button)
                .map(|(_, button)| button.get_rect()),
            [1] => Some(self.canvas_rect),
            [2] if !self.status.is_empty() => Some(self.status_rect),
            _ => None,
        }
    }

    /// The open photograph and the edits it supports, published to whatever is
    /// hosting this widget.
    ///
    /// The same tree [`crate::actions::PhotosResources`] serves to a packaged
    /// instance over the socket, so an app compiled into the compositor and one
    /// running as its own process offer the identical surface at the identical
    /// addresses. Without this the tree existed but only a caller already
    /// inside this process could reach it.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        use qos_actions::ResourceProvider;
        crate::actions::PhotosResources.resources(doc)
    }

    /// Keep the palette projection derived from the same fresh document
    /// resource tree as agents and assistive technology. The provider's
    /// catalog gives Ctrl+K Unicode-safe, cursor-paged bounded queries.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl PhotosApp {
    fn is_toolbar_hit_enabled(&self, hit: Hit) -> bool {
        self.buttons
            .iter()
            .find(|(candidate, _)| *candidate == hit)
            .is_some_and(|(_, button)| button.is_enabled())
    }

    fn focused_compact_more_hit(&self) -> Option<Hit> {
        COMPACT_MORE_ENTRIES.get(self.compact_more_focus).copied()
    }

    fn step_compact_more_focus(&mut self, forward: bool) {
        for _ in 0..COMPACT_MORE_ENTRIES.len() {
            self.compact_more_focus = if forward {
                (self.compact_more_focus + 1) % COMPACT_MORE_ENTRIES.len()
            } else {
                (self.compact_more_focus + COMPACT_MORE_ENTRIES.len() - 1)
                    % COMPACT_MORE_ENTRIES.len()
            };
            if self
                .focused_compact_more_hit()
                .is_some_and(|hit| self.is_toolbar_hit_enabled(hit))
            {
                break;
            }
        }
    }

    /// Draw the phone-only action menu over the current surface. Its entries
    /// reuse the canonical toolbar actions, so hiding their buttons never
    /// creates a second edit/export implementation.
    fn draw_compact_more(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.compact_more_hits.clear();
        if !self.compact_more_open || self.toolbar_slots() != COMPACT_SLOTS {
            return;
        }
        let entries = [
            (Hit::CropToView, tr("photos-crop")),
            (Hit::Reset, tr("photos-reset")),
            (Hit::Copy, tr("photos-copy")),
            (Hit::Share, tr("photos-share")),
            (Hit::Save, tr("photos-save")),
        ];
        let width = 178.0;
        let row_h = 44.0;
        let panel = Rect::new(
            self.toolbar_rect.x1 - width - f64::from(PAD),
            self.toolbar_rect.y1 + 4.0,
            self.toolbar_rect.x1 - f64::from(PAD),
            self.toolbar_rect.y1 + 4.0 + row_h * entries.len() as f64,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 1.0),
        );
        for (index, (hit, label)) in entries.into_iter().enumerate() {
            let row = Rect::new(
                panel.x0,
                panel.y0 + index as f64 * row_h,
                panel.x1,
                panel.y0 + (index + 1) as f64 * row_h,
            );
            let enabled = self
                .buttons
                .iter()
                .find(|(candidate, _)| *candidate == hit)
                .is_some_and(|(_, button)| button.is_enabled());
            if index == self.compact_more_focus && enabled {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.bg_hover,
                    None,
                    &row,
                );
            }
            text_renderer.draw_text_weighted(
                scene,
                &label,
                13.0,
                if enabled {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 13.0)),
                Some((row.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            if index > 0 {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(row.x0, row.y0, row.x1, row.y0 + 1.0),
                );
            }
            if enabled {
                self.compact_more_hits.push((hit, row));
            }
        }
    }
}

/// Lets the compositor host Photos in a tab directly, the same way it hosts
/// the terminal, the file explorer and the word processor, rather than
/// standing a placeholder card where the photograph should be.
impl ui::widget::AppWidget for PhotosApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's tab vocabulary has no picture variant. Photos edits a
        // file and writes the result back, which is what `Editor` names; the
        // glyph below is what actually tells the two apart in a tab pill.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        // The open file's own name, so two photographs open at once are two
        // distinguishable tabs rather than two tabs called "Photos". Nothing
        // open falls back to the app's name, which is the only true thing to
        // say about an empty window.
        self.open_image_path()
            .and_then(|p| p.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .unwrap_or("Photos")
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font image glyph, matching the convention every other hosted
        // app's tab pill uses.
        ui::widget::IconDescriptor::Glyph("\u{F02E9}")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{PHOTOS_OPERATION_TYPE, PhotosOperation, PhotosResources};
    use crate::model::{Album, LibraryPhoto};
    use crate::shots_pattern;
    use automerge::ROOT;
    use qos_actions::ResourceProvider;

    fn has_resource_id(resource: &qos_actions::Resource, id: &str) -> bool {
        resource.id.0 == id
            || resource
                .children
                .iter()
                .any(|child| has_resource_id(child, id))
    }

    #[test]
    fn edited_content_effect_stores_and_returns_the_exact_transformed_png() {
        let original = pixels::Rgba::new(
            3,
            2,
            vec![
                1, 10, 0, 255, 2, 10, 0, 255, 3, 10, 0, 255, 1, 20, 0, 255, 2, 20, 0, 255, 3, 20,
                0, 255,
            ],
        )
        .expect("asymmetric source");
        let source_png = pixels::encode_png(&original).expect("source PNG");
        let source_hash = qdoc::blobstore::put(&source_png).expect("stage source content");
        let open = OpenImage::from_path("fixture.png", 3, 2).with_content(source_hash.clone());
        let edits = crate::model::Edits {
            crop: crate::model::Rect01 {
                x: 1.0 / 3.0,
                y: 0.0,
                w: 2.0 / 3.0,
                h: 1.0,
            },
            rotate_quarters: 1,
        };
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_image(&mut doc, Some(&open));
        PhotosDoc::set_edits(&mut doc, &edits);
        let resource_id = crate::actions::resource_id(&open);

        let resources = PhotosResources.resources(&doc);
        let exported = qos_actions::flatten(&resources)
            .into_iter()
            .find(|entry| {
                entry.resource_id.as_str() == resource_id
                    && entry.action.id == crate::actions::EXPORT_EDITED_CONTENT_ACTION_ID
            })
            .expect("addressed image exports edited content");
        assert_eq!(exported.action.kind, qos_actions::OperationKind::Effect);
        assert_eq!(
            exported.action.contract.retry,
            qos_actions::RetryPolicy::Idempotent
        );
        assert_eq!(
            exported.action.contract.result,
            qos_actions::ResultSpec::Resource {
                resource_kind: qos_actions::CONTENT_RESOURCE_KIND.into(),
            }
        );

        let payload = qos_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ExportEditedContent {
                resource_id: resource_id.clone(),
                source_content: source_hash.clone(),
                edits,
            },
        )
        .expect("export operation encodes");
        let mut app = PhotosApp::new();
        let started = Widget::start_resource_operation(
            &mut app,
            qos_actions::InvocationId::new(),
            qos_actions::OperationKind::Effect,
            PHOTOS_OPERATION_TYPE,
            &payload.payload,
            &doc,
            &ROOT,
        )
        .expect("export succeeds")
        .expect("Photos handles its operation");
        let ui::widget::ResourceOperationStart::Completed(output) = started else {
            panic!("bounded content production completes with its live output");
        };
        assert_eq!(output.resources.len(), 1);
        let produced_hash =
            qos_actions::content_hash(&output.resources[0]).expect("one canonical content result");
        assert_ne!(
            produced_hash, source_hash,
            "edited bytes must not reuse the source hash"
        );
        let stored = qdoc::blobstore::get(produced_hash).expect("produced bytes are retrievable");
        let decoded = pixels::decode(&stored).expect("produced PNG decodes");
        assert_eq!(
            decoded,
            pixels::render(&original, edits.crop, edits.rotate_quarters)
        );

        PhotosDoc::set_edits(&mut doc, &crate::model::Edits::default());
        assert!(
            Widget::start_resource_operation(
                &mut app,
                qos_actions::InvocationId::new(),
                qos_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
                &doc,
                &ROOT,
            )
            .is_err(),
            "a stale edit snapshot must not produce different content"
        );
    }

    #[test]
    fn edited_content_effect_rejects_missing_or_misdirected_source_bytes() {
        let missing = "f".repeat(64);
        let open = OpenImage::from_path("missing.png", 3, 2).with_content(missing.clone());
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_image(&mut doc, Some(&open));
        let edits = PhotosDoc::edits(&doc);
        let encoded = |resource_id: String| {
            qos_actions::EncodedAction::encode_operation(
                PHOTOS_OPERATION_TYPE,
                &PhotosOperation::ExportEditedContent {
                    resource_id,
                    source_content: missing.clone(),
                    edits,
                },
            )
            .expect("export operation encodes")
        };
        let mut app = PhotosApp::new();
        for payload in [
            encoded(crate::actions::resource_id(&open)),
            encoded("photos.image:wrong-target".into()),
        ] {
            assert!(
                Widget::start_resource_operation(
                    &mut app,
                    qos_actions::InvocationId::new(),
                    qos_actions::OperationKind::Effect,
                    PHOTOS_OPERATION_TYPE,
                    &payload.payload,
                    &doc,
                    &ROOT,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn opening_an_album_operation_only_changes_the_local_filter_and_refuses_stale_ids() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.photos.push(photo.clone());
        library.albums.push(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: vec![photo.id],
        });
        PhotosDoc::set_library(&mut doc, &library);
        let before = PhotosDoc::library(&doc);
        let payload = qos_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:trip".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                qos_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert!(app.library_mode);
        assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "navigation must not alter membership"
        );

        let stale = qos_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:gone".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                qos_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &stale.payload,
                &doc,
                &ROOT,
            )
            .is_err()
        );
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    #[test]
    fn unresolved_share_reference_returns_a_canonical_operation_failure() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let payload = qos_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ShareReference {
                resource_id: "photos.photo:missing".into(),
            },
        )
        .expect("share operation encodes");

        let error = Widget::invoke_resource_operation(
            &mut app,
            qos_actions::OperationKind::Effect,
            PHOTOS_OPERATION_TYPE,
            &payload.payload,
            &doc,
            &ROOT,
        )
        .expect_err("a missing resource cannot claim that its reference was shared");
        assert_eq!(error, tr("photos-share-failed"));
        assert_eq!(
            app.notice.as_deref(),
            Some(tr("photos-share-failed").as_str())
        );
    }

    #[test]
    fn metadata_views_are_local_filters_with_honest_stale_results() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut in_folder =
            LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        in_folder.folder = Some("Travel".into());
        in_folder.people = vec!["Élodie".into()];
        in_folder.tags = vec!["golden-hour".into()];
        let other = LibraryPhoto::from_image(OpenImage::from_path("/photos/two.png", 20, 10));
        let library = crate::model::PhotoLibrary {
            photos: vec![in_folder, other],
            ..Default::default()
        };
        PhotosDoc::set_library(&mut doc, &library);

        app.view_metadata_filter(&doc, Some("Travel".into()), None, None)
            .expect("existing folder enters a view");
        app.sync(&mut doc, &ROOT);
        assert!(app.library_mode);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_folder.as_deref(), Some("Travel"));
        assert!(app.active_person.is_none());

        app.view_metadata_filter(&doc, None, Some("Élodie".into()), None)
            .expect("existing person enters a view");
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_person.as_deref(), Some("Élodie"));
        assert!(app.active_folder.is_none());
        app.view_metadata_filter(&doc, None, None, Some("golden-hour".into()))
            .expect("existing tag enters a view");
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.library_total, 1);
        assert_eq!(app.active_tag.as_deref(), Some("golden-hour"));
        assert!(app.active_folder.is_none());
        assert!(app.active_person.is_none());
        assert!(
            app.view_metadata_filter(&doc, Some("Missing".into()), None, None)
                .is_err()
        );
    }

    #[test]
    fn accessibility_exposes_and_routes_every_toolbar_control() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        app.sync(&mut doc, &ROOT);

        let empty = app.accessibility_tree();
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.children[0].role, Role::Toolbar);
        assert_eq!(
            empty.children[0].children.len(),
            SLOTS
                .iter()
                .filter(|slot| matches!(slot, Slot::Item(_)))
                .count()
        );
        assert_eq!(empty.children[1].role, Role::Image);
        assert_eq!(
            empty.children[0].children[0].action_target.as_deref(),
            Some("photos:toolbar:photos-open")
        );
        assert!(
            empty.children[0]
                .children
                .iter()
                .find(|node| {
                    node.action_target.as_deref() == Some("photos:toolbar:photos-rotate-right")
                })
                .is_some_and(|node| node.state.disabled),
            "rotate must be unavailable without a photograph"
        );
        assert!(
            !app.perform_toolbar_semantic_action(
                "photos:toolbar:photos-rotate-right",
                &AccessAction::Activate,
                &mut doc,
            ),
            "a disabled semantic control must reject activation"
        );

        let image = OpenImage::from_path("/tmp/accessible.png", 100, 50);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.original = Some(("/tmp/accessible.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        app.refresh(&doc);
        app.sync(&mut doc, &ROOT);
        assert!(Widget::perform_semantic_action(
            &mut app,
            "photos:toolbar:photos-rotate-right",
            &AccessAction::Activate,
            &mut doc,
            &ROOT,
        ));
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
        assert!(
            app.take_committed_action().is_some(),
            "the semantic route must create the normal undo boundary"
        );

        app.sync(&mut doc, &ROOT);
        let opened = app.accessibility_tree();
        assert_eq!(
            opened.children[1].name.as_deref(),
            Some("accessible.png — edited")
        );
        let status = opened
            .children
            .last()
            .expect("opened image has status text");
        assert_eq!(status.role, Role::StaticText);
        assert_eq!(status.live, Some(Politeness::Polite));
    }

    #[test]
    fn compact_more_is_a_keyboard_menu_with_a_share_action() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/tmp/compact.png", 100, 50);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.original = Some(("/tmp/compact.png".into(), shots_pattern(100, 50)));
        app.original_identity = Some(decode_key(&image));
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        app.sync(&mut doc, &ROOT);
        app.compact_more_open = true;

        // Crop and Reset start disabled, so Down must land on Copy rather
        // than trapping a keyboard user on unavailable commands.
        app.step_compact_more_focus(true);
        assert_eq!(app.focused_compact_more_hit(), Some(Hit::Copy));
        app.step_compact_more_focus(true);
        assert_eq!(app.focused_compact_more_hit(), Some(Hit::Share));

        let toolbar = app.toolbar_semantics();
        let menu = toolbar
            .children
            .iter()
            .find(|node| node.role == Role::Menu)
            .expect("narrow overflow exposes a menu");
        assert!(menu.children.iter().any(|item| {
            item.action_target.as_deref() == Some("photos:toolbar:photos-share")
                && item.state.focused
                && item.state.focusable
        }));
    }

    #[test]
    fn metadata_sheet_focus_is_a_six_stop_keyboard_cycle() {
        let mut app = PhotosApp::new();
        for focus in 0..6 {
            app.set_metadata_editor_focus(focus);
            assert_eq!(app.metadata_editor_focus, focus);
            assert_eq!(app.metadata_folder_input.is_focused(), focus == 0);
            assert_eq!(app.metadata_people_input.is_focused(), focus == 1);
            assert_eq!(app.metadata_tags_input.is_focused(), focus == 2);
            assert_eq!(app.metadata_captured_at_input.is_focused(), focus == 3);
        }
        app.set_metadata_editor_focus(99);
        assert_eq!(app.metadata_editor_focus, 5);
        assert!(
            !app.metadata_folder_input.is_focused()
                && !app.metadata_people_input.is_focused()
                && !app.metadata_tags_input.is_focused()
                && !app.metadata_captured_at_input.is_focused(),
            "button focus must leave a single semantic keyboard owner"
        );
    }

    #[test]
    fn unavailable_bytes_and_bad_bytes_have_distinct_honest_canvas_states() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let waiting =
            OpenImage::from_path("/not-present/yet.png", 1200, 800).with_content("0".repeat(64));
        PhotosDoc::set_image(&mut doc, Some(&waiting));
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.canvas_state, CanvasState::Loading);
        assert!(!app.has_image());
        assert_eq!(app.empty_state_text().0, tr("photos-loading-title"));
        assert!(
            app.buttons
                .iter()
                .filter(|(hit, _)| !matches!(hit, Hit::Open | Hit::Library))
                .all(|(_, button)| !button.is_enabled())
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.png");
        std::fs::write(&path, b"not image data").unwrap();
        let bad = OpenImage::from_path(&path.to_string_lossy(), 1200, 800);
        PhotosDoc::set_image(&mut doc, Some(&bad));
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.canvas_state, CanvasState::Error);
        assert_eq!(app.empty_state_text().0, tr("photos-error-title"));
    }

    #[test]
    fn synchronized_app_registers_the_canonical_photo_search_provider() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/pictures/München-夏.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.sync(&mut doc, &ROOT);
        let before = app.search_provider.stats();
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.search_provider.stats(),
            before,
            "unchanged sync rebuilt search"
        );

        let providers = Widget::search_providers(&app);
        assert_eq!(providers.len(), 1);
        let hit = providers[0].search("夏").pop().unwrap();
        let address = hit.action_address().unwrap();
        assert!(matches!(address.action.as_str(), "view" | "open"));
        assert!(
            PhotosResources
                .resources(&doc)
                .iter()
                .any(|resource| has_resource_id(resource, &address.resource))
        );
    }

    #[test]
    fn a_large_library_schedules_only_one_background_thumbnail_at_a_time() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..500 {
            library
                .photos
                .push(LibraryPhoto::from_image(OpenImage::from_path(
                    &format!("/unavailable/library-{index}.png"),
                    32_000,
                    32_000,
                )));
        }
        PhotosDoc::set_library(&mut doc, &library);
        app.sync(&mut doc, &ROOT);

        assert_eq!(app.thumbnail_visible_ids.len(), 24);
        assert!(app.thumbnail_queued.len() <= 1);
        assert!(app.library_thumbnails.len() <= 1);
        assert!(
            app.thumbnail_generation
                .is_some_and(|generation| generation > 0)
        );
    }

    #[test]
    fn album_editor_pages_large_libraries_without_changing_shared_membership() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..49 {
            library
                .photos
                .push(LibraryPhoto::from_image(OpenImage::from_path(
                    &format!("/photos/library-{index}.png"),
                    20,
                    10,
                )));
        }
        PhotosDoc::set_library(&mut doc, &library);
        app.library_total = library.photos.len();
        app.library_mode = true;
        app.album_editor = true;
        app.canvas_rect = Rect::new(0.0, 0.0, 390.0, 760.0);
        let before = PhotosDoc::library(&doc);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let (_, previous) = app
            .album_page_buttons
            .iter()
            .find(|(hit, _)| *hit == AlbumEditorHit::PreviousPage)
            .expect("the editor has a previous-page control");
        let (_, next) = app
            .album_page_buttons
            .iter()
            .find(|(hit, _)| *hit == AlbumEditorHit::NextPage)
            .expect("the editor has a next-page control");
        assert!(!previous.is_enabled());
        assert!(next.is_enabled());
        let semantics = app.accessibility_tree();
        let pager = &semantics.children[1].children;
        assert!(pager.iter().any(|node| {
            node.action_target.as_deref() == Some("photos:album-editor:previous-page")
                && node.state.disabled
                && node.name.as_deref() == Some("Show previous photographs")
        }));
        assert!(pager.iter().any(|node| {
            node.action_target.as_deref() == Some("photos:album-editor:next-page")
                && !node.state.disabled
                && node.name.as_deref() == Some("Show next photographs")
        }));

        assert!(app.perform_library_semantic_action(
            "photos:album-editor:next-page",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.library_offset, 24);
        assert!(app.perform_library_semantic_action(
            "photos:album-editor:previous-page",
            &AccessAction::Activate,
            &mut doc,
        ));
        // The preceding activation is valid from the second page and leaves
        // the editor's membership draft local.
        assert_eq!(app.library_offset, 0);

        app.page_library(true);
        assert_eq!(app.library_offset, 24);
        app.page_library(true);
        assert_eq!(app.library_offset, 48);
        app.page_library(true);
        assert_eq!(app.library_offset, 48);
        app.page_library(false);
        assert_eq!(app.library_offset, 24);
        app.page_library(false);
        assert_eq!(app.library_offset, 0);
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    #[test]
    fn a_stale_thumbnail_result_cannot_populate_a_replaced_library_slice() {
        let mut app = PhotosApp::new();
        app.thumbnail_generation = Some(8);
        app.thumbnail_visible_ids = vec!["photos.photo:current".into()];
        app.thumbnail_queued.insert("photos.photo:current".into());
        app.accept_thumbnail_result(ThumbnailResult {
            generation: 7,
            id: "photos.photo:current".into(),
            preview: Some(shots_pattern(16, 8)),
        });
        assert!(app.library_thumbnails.is_empty());
        assert!(app.thumbnail_queued.contains("photos.photo:current"));

        app.accept_thumbnail_result(ThumbnailResult {
            generation: 8,
            id: "photos.photo:current".into(),
            preview: Some(shots_pattern(16, 8)),
        });
        assert!(app.library_thumbnails.contains_key("photos.photo:current"));
        assert!(!app.thumbnail_queued.contains("photos.photo:current"));
    }

    #[test]
    fn exhausted_thumbnail_generation_fails_closed_without_reusing_a_token() {
        let mut app = PhotosApp::new();
        app.thumbnail_generation = Some(u64::MAX);
        app.thumbnail_visible_ids = vec!["photos.photo:before".into()];
        app.library_thumbnails.insert(
            "photos.photo:before".into(),
            to_peniko(&shots_pattern(16, 8)),
        );
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/after.png", 16, 8));
        let id = photo.id.clone();

        app.schedule_library_thumbnail(vec![photo], true);
        assert_eq!(app.thumbnail_generation, None);
        assert!(app.thumbnail_queued.is_empty());
        assert!(app.library_thumbnails.is_empty());

        app.accept_thumbnail_result(ThumbnailResult {
            generation: u64::MAX,
            id,
            preview: Some(shots_pattern(16, 8)),
        });
        assert!(app.library_thumbnails.is_empty());
    }

    #[test]
    fn library_removal_requires_confirmation_and_routes_through_the_action_host() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/remove.png", 20, 10));
        let id = photo.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: Vec::new(),
            },
        );
        app.show_library();
        app.sync(&mut doc, &ROOT);
        let target = format!("photos:library-remove:{id}");
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &mut doc));
        assert!(PhotosDoc::library(&doc).photo(&id).is_some());
        assert_eq!(app.library_remove_confirm.as_deref(), Some(id.as_str()));
        assert!(app.perform_library_semantic_action(&target, &AccessAction::Activate, &mut doc));
        assert!(PhotosDoc::library(&doc).photo(&id).is_none());
        assert!(app.take_committed_action().is_some());
    }

    #[test]
    fn high_text_grid_uses_readable_card_measures_and_a_single_sheet_lane() {
        let width = 1_440.0;
        assert_eq!(library_column_count(width, true, false), 2);
        assert_eq!(library_column_count(width, true, true), 1);
        let two_column_card_width = (width - f64::from(PAD * 2.0) - 8.0) / 2.0;
        assert!(
            two_column_card_width >= 600.0,
            "200% text needs a real reading measure, got {two_column_card_width}"
        );
    }

    /// 2026-09-16 alignment review: at 200% text the toolbar's 34 px capsules
    /// no longer held their 28 px labels and the 28.8 px status band clipped
    /// the filename (plan task 19). Both grow from the label line the shared
    /// renderer will draw — and stay exactly the authored size at 1×.
    #[test]
    fn toolbar_capsules_and_status_band_contain_their_text_at_any_scale() {
        assert_eq!(btn_h(1.0), BTN_H, "1× keeps the authored capsule");
        assert_eq!(status_h(1.0), BAR_H * 0.6, "1× keeps the authored band");
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let (_, label_line) = tr.measure("More", BUTTON_LABEL_SIZE, None);
        let (_, status_line) = tr.measure("harbour.png · 1200 × 800", STATUS_TEXT_SIZE, None);
        assert!(
            btn_h(2.0) >= label_line + 2.0 * BTN_PAD_Y,
            "2× capsule {} contains a {label_line} px label",
            btn_h(2.0)
        );
        assert!(
            status_h(2.0) >= status_line + 2.0 * STATUS_PAD_Y,
            "2× band {} contains a {status_line} px line",
            status_h(2.0)
        );
        // Icon-only items stay square at every scale.
        assert_eq!(item_width(Face::IconOnly, "", btn_h(2.0)), btn_h(2.0));
    }

    /// 2026-09-17 review: at 200% on an 800×900 window the details sheet was
    /// capped at the viewport while its fields kept a `60 × scale` pitch and
    /// the footer stayed bottom-anchored, so Cancel / Save painted over the
    /// "Captured" field. The field list now scrolls under the pinned footer:
    /// focusing the last field reveals it inside the visible body, and the
    /// footer always sits below that body.
    #[test]
    fn metadata_sheet_footer_never_covers_a_focused_field() {
        for (scale, (w, h)) in [(1.0_f32, (1200.0_f32, 800.0_f32)), (2.0, (800.0, 900.0))] {
            let mut app = PhotosApp::new();
            let mut doc = AutoCommit::new();
            app.show_library();
            let staging = tempfile::tempdir().expect("staging dir");
            crate::seed_scenario(&mut app, &mut doc, "metadata-edit", staging.path());
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(theme.text_scale);
            for frame in 0..3 {
                if frame == 1 {
                    assert!(
                        app.metadata_editor,
                        "the metadata-edit scenario opens the sheet"
                    );
                    app.set_metadata_editor_focus(3);
                }
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                app.sync(&mut doc, &automerge::ROOT);
                let child = app.build_layout(&mut taffy, &theme, &mut node_map);
                let root = taffy
                    .new_with_children(
                        Style {
                            size: Size {
                                width: Dimension::length(w),
                                height: Dimension::length(h),
                            },
                            ..Default::default()
                        },
                        &[child],
                    )
                    .unwrap();
                let measure_map = node_map.clone();
                let trr = &mut tr;
                taffy
                    .compute_layout_with_measure(
                        root,
                        Size {
                            width: taffy::AvailableSpace::Definite(w),
                            height: taffy::AvailableSpace::Definite(h),
                        },
                        |known, available, id, _, _| {
                            measure_map.get(&id).map_or(Size::ZERO, |&p| unsafe {
                                (*p).measure(known, available, trr)
                            })
                        },
                    )
                    .unwrap();
                app.apply_layout(&taffy, child, Point::ORIGIN);
                let mut scene = Scene::new();
                app.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &ui::presence::PresenceCtx::empty(),
                );
            }
            let body = app.metadata_editor_body;
            let captured = app.metadata_captured_at_input.get_rect();
            assert!(
                captured.y0 >= body.y0 - 0.01 && captured.y1 <= body.y1 + 0.01,
                "{scale}×: the focused Captured field {captured:?} is inside the visible body {body:?}"
            );
            assert!(
                app.metadata_editor_hits.len() == 2
                    && app
                        .metadata_editor_hits
                        .iter()
                        .all(|(_, rect)| rect.y0 >= body.y1 - 0.01),
                "{scale}×: Cancel and Save sit below the body {body:?}: {:?}",
                app.metadata_editor_hits
                    .iter()
                    .map(|(_, rect)| *rect)
                    .collect::<Vec<_>>()
            );
            if (scale - 1.0).abs() < f32::EPSILON {
                assert_eq!(
                    app.metadata_editor_scroll, 0.0,
                    "1× holds the whole sheet without scrolling"
                );
            } else {
                assert!(
                    app.metadata_editor_scroll > 0.0,
                    "2× on 800×900 needs the field list to scroll to reveal Captured"
                );
            }
        }
    }

    /// Library card action rows keep the same inset from both card edges, so
    /// Details on the left and the favourite star on the right read as one
    /// row (the star cluster used to run flush to `card.x1`).
    #[test]
    fn library_card_actions_share_one_edge_inset() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        app.show_library();
        let staging = tempfile::tempdir().expect("staging dir");
        crate::seed_scenario(&mut app, &mut doc, "library", staging.path());
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        // Two frames: the first lays out roomy, the second refits the toolbar
        // to the width it produced (see `build_layout`).
        for _ in 0..2 {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            app.sync(&mut doc, &automerge::ROOT);
            let child = app.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::length(1200.0),
                            height: Dimension::length(800.0),
                        },
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    Size {
                        width: taffy::AvailableSpace::Definite(1200.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                    |known, available, id, _, _| {
                        measure_map.get(&id).map_or(Size::ZERO, |&p| unsafe {
                            (*p).measure(known, available, trr)
                        })
                    },
                )
                .unwrap();
            app.apply_layout(&taffy, child, Point::ORIGIN);
            let mut scene = Scene::new();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &ui::presence::PresenceCtx::empty(),
            );
        }
        let rects = app.library_card_action_rects();
        assert!(!rects.is_empty(), "the library scenario paints cards");
        for (card, details, _remove, star) in &rects {
            let left_inset = details.x0 - card.x0;
            let right_inset = card.x1 - star.x1;
            assert!(
                (left_inset - right_inset).abs() < 0.5,
                "card {card:?}: Details inset {left_inset} vs star inset {right_inset}"
            );
        }
    }
}

#[cfg(test)]
mod history_reconcile_tests {
    use super::*;
    use automerge::ROOT;
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
    fn setup() -> (PhotosApp, AutoCommit, String) {
        let mut doc = AutoCommit::new();
        let image = OpenImage::from_path("historical-missing.png", 4, 3);
        PhotosDoc::set_image(&mut doc, Some(&image));
        let id = PhotosDoc::library(&doc).photos[0].id.clone();
        let mut app = PhotosApp::new();
        app.sync(&mut doc, &ROOT);
        (app, doc, id)
    }
    #[test]
    fn changed_target_invalidates_metadata_draft_before_input_but_unrelated_change_preserves_it() {
        let (mut app, mut doc, id) = setup();
        let mut lib = PhotosDoc::library(&doc);
        lib.photos[0].tags = vec!["restored".into()];
        PhotosDoc::set_library(&mut doc, &lib);
        let selected = doc.get_heads();
        lib.photos[0].tags = vec!["live".into()];
        PhotosDoc::set_library(&mut doc, &lib);
        app.sync(&mut doc, &ROOT);
        app.begin_metadata_editor(&doc, &id);
        app.metadata_tags_input.set_text("unsaved draft");
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        app.sync(&mut doc, &ROOT);
        assert!(app.metadata_editor);
        assert_eq!(app.metadata_tags_input.value(), "unsaved draft");
        restore(&mut doc, &selected);
        app.perform_semantic_action("nonexistent", &AccessAction::Activate, &mut doc, &ROOT);
        assert!(
            !app.metadata_editor,
            "stale draft must detach before any new input"
        );
        app.save_metadata_editor(&mut doc);
        assert_eq!(PhotosDoc::library(&doc).photos[0].tags, vec!["restored"]);
        app.begin_metadata_editor(&doc, &id);
        app.metadata_tags_input.set_text("next edit");
        app.save_metadata_editor(&mut doc);
        assert_eq!(PhotosDoc::library(&doc).photos[0].tags, vec!["next edit"]);
    }
    #[test]
    fn changed_photo_identity_invalidates_cached_thumbnail_and_late_old_result() {
        let (mut app, mut doc, id) = setup();
        let old = app.thumbnail_generation.unwrap();
        app.library_thumbnails
            .insert(id.clone(), to_peniko(&crate::shots_pattern(4, 3)));
        let mut lib = PhotosDoc::library(&doc);
        lib.photos[0].image.content = Some("a".repeat(64));
        PhotosDoc::set_library(&mut doc, &lib);
        app.sync(&mut doc, &ROOT);
        assert!(!app.library_thumbnails.contains_key(&id));
        app.accept_thumbnail_result(ThumbnailResult {
            generation: old,
            id: id.clone(),
            preview: Some(crate::shots_pattern(4, 3)),
        });
        assert!(!app.library_thumbnails.contains_key(&id));
    }
    #[test]
    fn pending_resource_open_cannot_overwrite_changed_content_and_settles_once() {
        let (mut app, mut doc, _) = setup();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("import.png");
        std::fs::write(
            &path,
            pixels::encode_png(&crate::shots_pattern(4, 3)).unwrap(),
        )
        .unwrap();
        let invocation = qos_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::PhotosOperation::Open {
            path: Some(path.to_string_lossy().into()),
        })
        .unwrap();
        app.start_resource_operation(
            invocation,
            qos_actions::OperationKind::Effect,
            crate::actions::PHOTOS_OPERATION_TYPE,
            &payload,
            &doc,
            &ROOT,
        )
        .unwrap();
        PhotosDoc::set_image(&mut doc, None);
        let heads = doc.get_heads();
        app.sync(&mut doc, &ROOT);
        assert!(PhotosDoc::image(&doc).is_none());
        assert_eq!(doc.get_heads(), heads);
        assert!(
            matches!(app.poll_resource_operation(invocation,false,&doc,&ROOT),Some(qos_actions::InvocationUpdate::Cancelled{invocation:id,..}) if id==invocation)
        );
        assert!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT)
                .is_none()
        );
    }
    #[test]
    fn already_dispatched_picker_reports_actual_outcome_without_importing_stale_selection() {
        for outcome in [0, 1, 2] {
            let (mut app, mut doc, _) = setup();
            let invocation = qos_actions::InvocationId::new();
            let (tx, rx) = std::sync::mpsc::channel();
            app.picker = Some(rx);
            app.resource_invocation = Some(invocation);
            app.import_content = Some(LiveContent::read(&doc));
            PhotosDoc::set_image(&mut doc, None);
            app.sync(&mut doc, &ROOT);
            tx.send(match outcome {
                0 => ui::portal::PortalOutcome::Opened(std::path::PathBuf::from("chosen.png")),
                1 => ui::portal::PortalOutcome::Cancelled,
                _ => ui::portal::PortalOutcome::Failed("actual picker failure".into()),
            })
            .unwrap();
            let heads = doc.get_heads();
            app.sync(&mut doc, &ROOT);
            assert_eq!(doc.get_heads(), heads);
            assert!(PhotosDoc::image(&doc).is_none());
            match app
                .poll_resource_operation(invocation, false, &doc, &ROOT)
                .unwrap()
            {
                qos_actions::InvocationUpdate::Failed {
                    invocation: id,
                    error,
                } => {
                    assert_eq!(id, invocation);
                    assert_eq!(
                        error.code,
                        if outcome == 0 {
                            "photos.stale_selection"
                        } else {
                            "photos.portal_failed"
                        }
                    );
                    if outcome == 0 {
                        assert!(error.message.contains("chosen.png"));
                    } else {
                        assert_eq!(error.message, "actual picker failure");
                    }
                }
                qos_actions::InvocationUpdate::Cancelled { invocation: id, .. } => {
                    assert_eq!(id, invocation);
                    assert_eq!(outcome, 1);
                }
                other => panic!("unexpected settlement {other:?}"),
            }
            assert!(
                app.poll_resource_operation(invocation, false, &doc, &ROOT)
                    .is_none()
            );
        }
    }
    #[test]
    fn changed_album_or_deleted_member_detaches_draft_and_cannot_resurrect_it() {
        let (mut app, mut doc, id) = setup();
        let selected = doc.get_heads();
        let mut lib = PhotosDoc::library(&doc);
        lib.albums.push(crate::model::Album {
            id: "album".into(),
            name: "Original".into(),
            photo_ids: vec![id],
        });
        PhotosDoc::set_library(&mut doc, &lib);
        app.sync(&mut doc, &ROOT);
        app.active_album = Some("album".into());
        app.begin_edit_album(&doc);
        app.album_name_input.set_text("stale draft");
        restore(&mut doc, &selected);
        app.sync(&mut doc, &ROOT);
        assert!(!app.album_editor);
        app.save_album_editor(&mut doc);
        assert!(PhotosDoc::library(&doc).albums.is_empty());
        app.begin_new_album(&doc);
        app.album_name_input.set_text("Next album");
        app.save_album_editor(&mut doc);
        assert_eq!(PhotosDoc::library(&doc).albums[0].name, "Next album");
    }

    fn album_membership_fixture() -> (PhotosApp, AutoCommit, [String; 4]) {
        let mut doc = AutoCommit::new();
        // Browse order deliberately differs from lexical identity order.
        let photos: Vec<_> = ["c", "a", "d", "b"]
            .into_iter()
            .map(|label| {
                LibraryPhoto::from_image(
                    OpenImage::from_path(&format!("{label}.png"), 4, 3)
                        .with_content(label.repeat(64)),
                )
            })
            .collect();
        let ids = std::array::from_fn(|i| photos[i].id.clone());
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos,
                albums: Vec::new(),
            },
        );
        let mut app = PhotosApp::new();
        app.library_mode = true;
        app.sync(&mut doc, &ROOT);
        (app, doc, ids)
    }

    fn activate_album_control(app: &mut PhotosApp, doc: &mut AutoCommit, target: &str) {
        assert!(app.perform_semantic_action(target, &AccessAction::Activate, doc, &ROOT));
        app.sync(doc, &ROOT);
    }

    fn seed_membership_album(doc: &mut AutoCommit, ids: &[String; 4]) {
        let mut library = PhotosDoc::library(doc);
        library.albums.push(crate::model::Album {
            id: "album".into(),
            name: "Original".into(),
            photo_ids: vec![ids[0].clone(), ids[3].clone()],
        });
        PhotosDoc::set_library(doc, &library);
    }

    #[test]
    fn album_membership_create_saves_selected_ids_in_browse_order() {
        let (mut app, mut doc, ids) = album_membership_fixture();
        activate_album_control(&mut app, &mut doc, "photos:album-editor:new");
        app.album_name_input.set_text("Selected photographs");
        // Select in reverse browse order; toggle a third photograph off again.
        for i in [3, 1, 0, 1] {
            activate_album_control(
                &mut app,
                &mut doc,
                &format!("photos:library-photo:{}", ids[i]),
            );
        }
        activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
        let library = PhotosDoc::library(&doc);
        assert_eq!(library.albums.len(), 1);
        assert_eq!(library.albums[0].name, "Selected photographs");
        assert_eq!(
            library.albums[0].photo_ids,
            vec![ids[0].clone(), ids[3].clone()]
        );
        assert!(!app.album_editor);
    }

    #[test]
    fn album_membership_rename_preserves_selected_ids_and_order() {
        let (mut app, mut doc, ids) = album_membership_fixture();
        seed_membership_album(&mut doc, &ids);
        app.sync(&mut doc, &ROOT);
        activate_album_control(&mut app, &mut doc, "photos:library-album:album");
        activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
        app.album_name_input.set_text("Renamed album");
        activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
        let library = PhotosDoc::library(&doc);
        assert_eq!(library.albums.len(), 1);
        let album = library.album("album").unwrap();
        assert_eq!(album.name, "Renamed album");
        assert_eq!(album.photo_ids, vec![ids[0].clone(), ids[3].clone()]);
        assert!(!app.album_editor);
    }

    #[test]
    fn album_membership_after_restore_supports_normal_edit_and_browse_order() {
        let (mut app, mut doc, ids) = album_membership_fixture();
        seed_membership_album(&mut doc, &ids);
        let selected = doc.get_heads();
        let mut library = PhotosDoc::library(&doc);
        library.albums[0].name = "Later album".into();
        library.albums[0].photo_ids = vec![ids[1].clone(), ids[2].clone()];
        PhotosDoc::set_library(&mut doc, &library);
        app.sync(&mut doc, &ROOT);
        activate_album_control(&mut app, &mut doc, "photos:library-album:album");
        activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
        app.album_name_input.set_text("Unsaved later draft");
        restore(&mut doc, &selected);
        app.sync(&mut doc, &ROOT);
        assert!(!app.album_editor, "restore must detach the obsolete draft");
        let restored = PhotosDoc::library(&doc);
        let album = restored.album("album").unwrap();
        assert_eq!(album.name, "Original");
        assert_eq!(album.photo_ids, vec![ids[0].clone(), ids[3].clone()]);

        activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
        app.album_name_input.set_text("Edited after restore");
        activate_album_control(&mut app, &mut doc, "photos:library-album:all");
        for i in [3, 2] {
            activate_album_control(
                &mut app,
                &mut doc,
                &format!("photos:library-photo:{}", ids[i]),
            );
        }
        activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
        let library = PhotosDoc::library(&doc);
        let album = library.album("album").unwrap();
        assert_eq!(album.name, "Edited after restore");
        assert_eq!(album.photo_ids, vec![ids[0].clone(), ids[2].clone()]);
        assert_eq!(library.photos.len(), 4);
        assert!(!app.album_editor);
    }

    #[test]
    fn content_restore_drops_active_viewer_drag_before_next_pointer_move() {
        let mut doc = AutoCommit::new();
        let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
        let open = OpenImage::from_path("same.png", 6, 4)
            .with_content(qdoc::blobstore::put(&bytes).unwrap());
        PhotosDoc::set_image(&mut doc, Some(&open));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let selected = doc.get_heads();
        PhotosDoc::set_edits(&mut doc, &Edits::default());
        let mut app = PhotosApp::new();
        app.sync(&mut doc, &ROOT);
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let theme = Theme::dark();
        let node = app.build_layout(&mut taffy, &theme, &mut nodes);
        taffy
            .set_style(
                node,
                Style {
                    size: Size {
                        width: Dimension::length(800.),
                        height: Dimension::length(600.),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(800.),
                    height: AvailableSpace::Definite(600.),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, Point::ZERO);
        let center = app.viewer.as_ref().unwrap().get_rect().center();
        let device_id = unsafe { std::mem::zeroed() };
        let mut handled = false;
        let mut focus = None;
        for event in [
            winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
            },
            winit::event::WindowEvent::MouseInput {
                device_id,
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
            },
        ] {
            app.viewer.as_mut().unwrap().handle_event(
                &event,
                1.,
                &winit::event::Modifiers::default(),
                &mut doc,
                &ROOT,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        }
        restore(&mut doc, &selected);
        app.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(center.x + 40., center.y + 20.),
            },
            1.,
            &winit::event::Modifiers::default(),
            &mut doc,
            &ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert_eq!(app.viewer.as_ref().unwrap().pan(), vello::kurbo::Vec2::ZERO);
        assert!((app.viewer.as_ref().unwrap().zoom() - 1.).abs() < f32::EPSILON);
        let next = PhotosDoc::edits(&doc).rotated_right();
        app.commit_edits(&mut doc, next);
        app.sync(&mut doc, &ROOT);
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 2);
    }

    #[test]
    fn failed_import_reports_failure_even_when_previous_photo_is_still_decoded() {
        let mut doc = AutoCommit::new();
        let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
        let open = OpenImage::from_path("existing.png", 6, 4)
            .with_content(qdoc::blobstore::put(&bytes).unwrap());
        PhotosDoc::set_image(&mut doc, Some(&open));
        let mut app = PhotosApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.open_image_path().is_some());
        let invocation = qos_actions::InvocationId::new();
        let tmp = tempfile::tempdir().unwrap();
        let payload = serde_json::to_vec(&crate::actions::PhotosOperation::Open {
            path: Some(tmp.path().join("absent.png").to_string_lossy().into()),
        })
        .unwrap();
        app.start_resource_operation(
            invocation,
            qos_actions::OperationKind::Effect,
            crate::actions::PHOTOS_OPERATION_TYPE,
            &payload,
            &doc,
            &ROOT,
        )
        .unwrap();
        app.sync(&mut doc, &ROOT);
        assert!(
            matches!(app.poll_resource_operation(invocation,false,&doc,&ROOT),Some(qos_actions::InvocationUpdate::Failed{invocation:id,..}) if id==invocation)
        );
        assert_eq!(PhotosDoc::image(&doc), Some(open));
    }
}
