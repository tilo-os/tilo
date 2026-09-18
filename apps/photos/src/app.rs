//! The root widget: a toolbar, the image canvas, and a status line.
//!
//! # Composition
//!
//! Every control here is a shared toolkit widget. The buttons are
//! [`ui::widget::Button`], the canvas is [`ui::widget::ImageViewer`], and the
//! nothing-open state is [`ui::widget::EmptyState`].
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::icons::{Icon, draw_icon};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{
    Button, DateIndexIntent, DateIndexMarker, DateIndexMarkerKind, DateIndexedScrollbar,
    EmptyState, ImageViewer, ScrollMetrics, SearchField, TextInput, Widget,
};
use winit::keyboard::{Key, NamedKey};

use crate::doc::PhotosDoc;
use crate::model::{Edits, LibraryPhoto, OpenImage, Rect01};
use crate::pixels::{self, Rgba};
use crate::strings::tr;

/// Toolbar height, and the status bar's. Both are chrome around the canvas,
/// which takes everything left over.
const BAR_H: f32 = 48.0;
const PAD: f32 = 12.0;

/// Button height. Deliberately shorter than the bar so the controls sit in
/// the strip rather than filling it edge to edge.
const BTN_H: f32 = 34.0;

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
const DIRECTORY_IMPORT_QUEUE: usize = 16;
const DIRECTORY_IMPORT_PER_FRAME: usize = 2;
const DIRECTORY_IMPORT_MAX_FILES: usize = 10_000;
const ANALYSIS_QUEUE_LIMIT: usize = 2;
/// One bounded materialized library page. This is intentionally independent
/// of the number of cards that happen to fit in a particular window.
const LIBRARY_PAGE_SIZE: usize = 24;

/// A four-column shelf is only useful while each photograph remains a
/// recognisable, comfortably targetable card. At tablet widths a two-column
/// shelf works well; below this a single card preserves independent Details,
/// favourite, and removal targets at the 44px touch minimum.
const LIBRARY_TWO_COLUMN_MAX_W: f64 = 720.0;
const LIBRARY_ONE_COLUMN_MAX_W: f64 = 460.0;
/// The gallery controls use the shared 44px minimum target with a little
/// breathing room. Keeping this one value in both paint and test geometry
/// prevents a visual refinement from silently shrinking an accessible hit.
const LIBRARY_ACTION_H: f64 = 48.0;

/// Presentation-only density for the chronological library.  It is deliberately
/// pure: a pinch and the visible +/- buttons must land on the same geometry.
/// The lower bound protects the 44 px card actions; the upper bound keeps a
/// photograph recognisable rather than turning the timeline into a file list.
pub fn timeline_column_count(width: f64, density: u8, large_text: bool) -> usize {
    if large_text && width < 620.0 {
        return 1;
    }
    let base = if width < LIBRARY_ONE_COLUMN_MAX_W {
        1
    } else if width < 720.0 {
        2
    } else if width < 1_080.0 {
        3
    } else {
        4
    };
    let density = usize::from(density.min(4));
    let maximum = if large_text || width < LIBRARY_TWO_COLUMN_MAX_W {
        // A phone and compact tablet need real card footers, not a density
        // mode that turns the timeline into horizontally clipped glyphs.
        // Pinch/zoom still adjusts the spaciousness of the same one/two
        // columns through row geometry and preserves the scroll anchor.
        2
    } else if width >= 1_440.0 {
        6
    } else {
        5
    };
    (base + density).min(maximum).max(1)
}

/// Bounded virtual window for a row of a long chronological collection.
/// Keeping this independent of painting prevents a large library from being
/// accidentally decoded or exposed to accessibility as one giant surface.
pub fn timeline_virtual_range(
    total: usize,
    first_row: usize,
    rows: usize,
    columns: usize,
) -> std::ops::Range<usize> {
    let columns = columns.max(1);
    let start = first_row.saturating_mul(columns).min(total);
    let end = start
        .saturating_add(rows.saturating_add(2).saturating_mul(columns))
        .min(total);
    start..end
}

/// Measured rows for the compact library masthead.  Compact and 200% text
/// share this source of truth so the count, selection affordance, density,
/// collection rail, and first date never occupy the same vertical space.
///
/// Text is painted on a baseline, so the row bounds deliberately include a
/// little extra leading rather than treating the baseline as the row's top.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CompactHeaderGeometry {
    title_y: f64,
    count_y: f64,
    controls: Rect,
    albums_y: f64,
    album_actions: Rect,
    timeline_y: f64,
}

fn compact_library_header_geometry(
    bounds: Rect,
    large_text: bool,
    has_albums: bool,
) -> CompactHeaderGeometry {
    let top = bounds.y0 + f64::from(PAD);
    if large_text {
        let controls_y = top + 68.0;
        let albums_y = controls_y + 56.0;
        let actions_y = albums_y + if has_albums { 54.0 } else { 0.0 } + 10.0;
        let album_actions = Rect::new(
            bounds.x0 + f64::from(PAD),
            actions_y,
            bounds.x1 - f64::from(PAD),
            actions_y + 44.0,
        );
        CompactHeaderGeometry {
            title_y: top,
            // Keep this deliberately short at 200%; a wrapped count makes
            // the next explicit controls row impossible to reason about.
            count_y: top + 34.0,
            controls: Rect::new(
                bounds.x0 + f64::from(PAD),
                controls_y,
                bounds.x0 + f64::from(PAD) + 142.0,
                controls_y + 44.0,
            ),
            albums_y,
            album_actions,
            timeline_y: album_actions.y1 + 10.0,
        }
    } else {
        let albums_y = top + 24.0;
        CompactHeaderGeometry {
            title_y: top,
            count_y: top + 3.0,
            controls: Rect::new(
                bounds.x1 - 118.0,
                bounds.y0 + 8.0,
                bounds.x1 - f64::from(PAD),
                bounds.y0 + 44.0,
            ),
            albums_y,
            album_actions: Rect::ZERO,
            timeline_y: albums_y + if has_albums { 38.0 } else { 0.0 } + 8.0,
        }
    }
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

struct AnalysisResult {
    invocation: tilo_actions::InvocationId,
    photo_id: String,
    content: Option<String>,
    result: AnalysisOutcome,
}

/// Per-device checkpoint for the sensitive People scan.  It contains only a
/// cursor and status, never face geometry, embeddings, crops, model output,
/// source paths, or a person name.  Keeping it outside Automerge makes resume
/// local to the owner who consented.
#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct PeopleScanProgress {
    next_offset: usize,
    completed_batches: u64,
    last_library_size: usize,
    status: String,
    last_error: Option<String>,
}

impl PeopleScanProgress {
    fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_ok()
            && let Ok(bytes) = serde_json::to_vec(self)
        {
            let _ = atomic_file::write(&path, &bytes);
        }
    }

    fn path() -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".local/state"))
            })?;
        Some(base.join("tilo/photos/people-scan.json"))
    }
}

enum AnalysisOutcome {
    Image(Result<crate::model::ImageAnalysis, String>),
    People(Result<crate::people::VerifiedFaceClusterBatch, String>),
}

struct AnalysisJob {
    invocation: tilo_actions::InvocationId,
    photo_id: String,
    content: Option<String>,
    image: OpenImage,
    kind: AnalysisJobKind,
}

/// All pixel intelligence shares exactly one bounded worker. Adding a new
/// analysis kind must not create another request thread or bypass the queue.
enum AnalysisJobKind {
    Ocr {
        language: String,
    },
    Vision {
        capability: tilo_vision::LocalVisionCapability,
    },
    People {
        capability: tilo_vision::LocalFaceCapability,
        images: Vec<(String, OpenImage)>,
    },
}

fn analysis_failure(
    invocation: tilo_actions::InvocationId,
    message: String,
) -> tilo_actions::InvocationUpdate {
    tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: "photos.analysis_failed".into(),
            message,
            details: None,
            retryable: true,
        },
    }
}

fn analysis_worker(rx: &Receiver<AnalysisJob>, tx: &SyncSender<AnalysisResult>) {
    while let Ok(job) = rx.recv() {
        if tx
            .send(AnalysisResult {
                invocation: job.invocation,
                photo_id: job.photo_id,
                content: job.content,
                result: match job.kind {
                    AnalysisJobKind::Ocr { language } => AnalysisOutcome::Image(
                        crate::analysis::recognize_text(&job.image, &language),
                    ),
                    AnalysisJobKind::Vision { capability } => AnalysisOutcome::Image(
                        crate::analysis::analyze_image(&job.image, &capability),
                    ),
                    AnalysisJobKind::People { capability, images } => {
                        AnalysisOutcome::People(crate::analysis::detect_people(images, &capability))
                    }
                },
            })
            .is_err()
        {
            return;
        }
    }
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

/// The local selection shelf. Selection is intentionally not a CRDT value:
/// collaborators should see the durable favourite/album result, never a
/// private cursor-like set that changes while somebody is deciding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionHit {
    Start,
    SelectAll,
    Clear,
    Favorite,
    ChooseAlbum,
    CancelDestinationChooser,
    ApplyDestination,
    PreviousDestinationPage,
    NextDestinationPage,
    AddToAlbum,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimelineHit {
    ZoomOut,
    ZoomIn,
}

/// The library's three primary browse destinations. This is presentation-only
/// and deliberately separated from durable albums/metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryDestination {
    Timeline,
    People,
    Places,
}

#[derive(Clone)]
struct PlaceItem {
    photo: LibraryPhoto,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlacesHit {
    Timeline,
    People,
    Places,
    ToggleList,
    ZoomIn,
    ZoomOut,
    Marker(String),
}

/// One chooser page always fits: six chips in a desktop row or a 3×2 compact
/// grid. The complete album collection remains available through paging.
const SELECTION_ALBUMS_PER_PAGE: usize = 6;

/// A compact materialised card.  The full library remains canonical in the
/// document; this carries just enough metadata to make the visible shelf
/// scannable without another document walk while painting.
#[derive(Clone)]
struct LibraryItem {
    id: String,
    name: String,
    favorite: bool,
    metadata: String,
    captured_at: String,
}

#[derive(Clone)]
struct MetadataFilterCard {
    folder: Option<String>,
    person: Option<String>,
    tag: Option<String>,
    /// A favourite view is a first-class, shared collection but selecting it
    /// remains local presentation state. Keep it in the same card model as
    /// the metadata filters so pointer, keyboard, and assistive technology
    /// all address the exact visible chip.
    favorites: bool,
    bounds: Rect,
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
/// Crop and Save reuse the closest shared semantic glyphs (`Scissors` and
/// `Download`). Rotation uses distinct clockwise/counter-clockwise glyphs so
/// icon-only compact controls remain distinguishable without a tooltip.
const TOOLBAR: &[(Hit, &str, Icon, Face)] = &[
    (Hit::Open, "photos-open", Icon::Folder, Face::Labelled),
    (Hit::Library, "photos-library", Icon::Image, Face::Labelled),
    (
        Hit::RotateLeft,
        "photos-rotate-left",
        Icon::RotateCcw,
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
fn item_width(face: Face, label: &str) -> f32 {
    match face {
        Face::IconOnly => BTN_H,
        Face::Labelled => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(BTN_H)
        }
        // The normal estimate is calibrated for 100% text.  At the supported
        // 200% screenshot scale the overflow label needs a real word-sized
        // slot, while every other command is represented by an accessible
        // icon in the compact menu.
        Face::LargeOverflow => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W * 2.0;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(BTN_H)
        }
    }
}

/// Width the whole strip needs, `faces` giving each item's presentation.
///
/// Exposed so a test can assert the strip fits a window rather than
/// discovering it does not by finding Save missing from a screenshot.
pub fn toolbar_width(faces: &[Face]) -> f32 {
    let mut w = PAD * 2.0;
    for (i, slot) in SLOTS.iter().enumerate() {
        if i > 0 {
            w += BTN_GAP;
        }
        w += match *slot {
            // Unreachable: `faces` and `TOOLBAR` are always built the same
            // length, one entry per `SLOTS::Item` index.
            Slot::Item(n) => match (faces.get(n), TOOLBAR.get(n)) {
                (Some(&face), Some(item)) => item_width(face, &tr(item.1)),
                _ => 0.0,
            },
            Slot::Separator => SEP_W,
            Slot::Spacer => 0.0,
        };
    }
    w
}

/// Each item's presentation at a given strip width.
///
/// The labelled items give up their words rather than the strip giving up an
/// item: a control that has shrunk to its glyph is still reachable, one that
/// has fallen off the edge is not.
fn faces_for_width(width: f32) -> Vec<Face> {
    let roomy: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
    // Width 0 is the first frame, before a layout has happened. Assume room;
    // the next frame corrects it with a real measurement.
    if width <= 0.0 || toolbar_width(&roomy) <= width {
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

/// The rendered image, and the inputs it was rendered from.
///
/// Keeping the inputs beside the output is what lets `sync` decide in a couple
/// of comparisons whether anything needs redoing.
struct Rendered {
    path: String,
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
    /// Test-only evidence that each visible batch route creates the exact
    /// envelope the corresponding published action descriptor creates.
    #[cfg(test)]
    last_library_action: Option<tilo_actions::EncodedAction>,
    /// Decoded original, before any edit. `None` until a file opens.
    original: Option<(String, Rgba)>,
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
    compact_more_open: bool,
    compact_more_focus: usize,
    compact_more_hits: Vec<(Hit, Rect)>,
    /// Portal channel while a file chooser is open.
    picker: Option<ui::portal::PortalChannel>,
    directory_picker: Option<ui::portal::PortalChannel>,
    directory_import: Option<DirectoryImport>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    pending_resource_open: Option<(tilo_actions::InvocationId, String)>,
    analysis_tx: Option<SyncSender<AnalysisJob>>,
    analysis_rx: Receiver<AnalysisResult>,
    active_analysis: HashSet<tilo_actions::InvocationId>,
    cancelled_analysis: HashSet<tilo_actions::InvocationId>,
    active_people_scans: HashSet<tilo_actions::InvocationId>,
    /// Private People-review state. It is intentionally neither a document
    /// field nor a resource: unconfirmed face proposals belong only on this
    /// device until an explicit review commits a compact label membership.
    people_controller: crate::people::PeopleController,
    /// Next library offset for the explicitly started People scan.  Keeping a
    /// cursor makes a large local library progress in 12-photo work units
    /// without ever submitting hundreds of biometric previews at once.
    people_scan_progress: PeopleScanProgress,
    /// A distinct People collection surface. Its private suggestion state is
    /// visually and semantically separated from shared approved labels.
    people_mode: bool,
    canvas_state: CanvasState,
    search_provider: crate::search::PhotosSearchProvider,
    /// Heads that produced the cached immutable/Arc-backed search catalogue.
    /// An unchanged `sync` must not rebuild a full-library index.
    search_heads: Vec<automerge::ChangeHash>,
    /// A rejected index generation never erases the last-good provider. This
    /// bounded diagnostic is visible and exported until a later refresh
    /// succeeds.
    search_refresh_error: Option<String>,
    #[cfg(test)]
    force_search_refresh_error: bool,
    /// The library is deliberately a bounded presentation slice.  The CRDT
    /// can hold considerably more entries than a window can paint or an
    /// assistive technology should receive in one update.
    library_items: Vec<LibraryItem>,
    library_total: usize,
    /// A local query over the current library projection.  The intelligence
    /// owner can extend this predicate with OCR and descriptions without
    /// changing the shared `SearchField` interaction contract.
    library_search: SearchField,
    library_query: String,
    library_destination: LibraryDestination,
    places_list_mode: bool,
    places_viewport: crate::places::MapViewport,
    places_items: Vec<PlaceItem>,
    places_hits: Vec<(PlacesHit, Rect)>,
    places_map_bounds: Rect,
    places_drag_origin: Option<Point>,
    /// Count for the always-addressable Favorites virtual collection. This is
    /// a scalar projection of the shared library, so paint never walks the
    /// document and the chip stays bounded even for a very large collection.
    library_favorite_count: usize,
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
    /// AI-derived object/pet collections use the same local-only navigation
    /// rule as tags. Their labels are present only after explicit sharing.
    active_object: Option<String>,
    active_pet: Option<String>,
    /// The stable Favorites resource is a view over shared favourite markers,
    /// never a private second list. Opening it is local; toggling a star is a
    /// typed shared document action.
    active_favorites: bool,
    /// Bounded labels used for discoverable visual filtering. Search and the
    /// resource tree still resolve every stable id outside this shelf.
    library_folders: Vec<(String, usize)>,
    library_people: Vec<(String, usize)>,
    /// One bounded representative image per approved person, selected from
    /// the shared metadata projection and rendered from the existing cache.
    library_person_covers: HashMap<String, String>,
    library_tags: Vec<(String, usize)>,
    metadata_filter_cards: Vec<MetadataFilterCard>,
    /// Visible paging affordances for a one-card enlarged-text shelf. These
    /// are local navigation only; their stable semantic targets let keyboard
    /// and assistive-technology users reach the same bounded slice.
    library_page_hits: Vec<(bool, bool, Rect)>,
    timeline_density_hits: Vec<(TimelineHit, Rect)>,
    timeline_density_buttons: Vec<(TimelineHit, Button)>,
    timeline_density_pending: Rc<Cell<Option<TimelineHit>>>,
    /// Retained shared controls over the bespoke timeline surface.  The
    /// date/photo canvas owns its rhythm; header actions still use toolkit
    /// button states, focus treatment, and accessible names.
    library_select_button: Button,
    library_album_buttons: Vec<(AlbumEditorHit, Button)>,
    /// Number of visual density steps, from spacious (0) to compact (4).
    /// This remains local UI state: collaborators should never change a
    /// person's preferred thumbnail size.
    timeline_density: u8,
    /// Fractional row offset: pixel-wheel scrolling is smooth while the
    /// materialised slice still advances only when its leading row changes.
    timeline_scroll_row: f64,
    timeline_last_columns: usize,
    /// Shared chronological navigation face.  It owns no library data: the
    /// current row offset/density remain private presentation state, while
    /// date boundaries are a bounded projection of canonical photo metadata.
    timeline_index: DateIndexedScrollbar,
    timeline_index_markers: Vec<DateIndexMarker>,
    timeline_index_metrics: ScrollMetrics,
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
    /// Local selection is an affordance for applying one canonical batch
    /// action. It is bounded to cards the current view has materialized.
    selection_mode: bool,
    selected_photo_ids: HashSet<String>,
    selection_anchor: Option<String>,
    /// The destination is an explicit local choice, never inferred from the
    /// album currently being browsed (which would commonly be a no-op).
    selection_destination_album: Option<String>,
    selection_album_chooser: bool,
    /// The chooser pages a bounded materialised slice rather than silently
    /// clipping albums beyond the available shelf height.
    selection_album_page: usize,
    /// Projection refreshed from the shared library; it lets the visible
    /// batch control say Favorite or Unfavorite truthfully without storing
    /// selection state in the document.
    selection_all_favorite: bool,
    selection_hits: Vec<(SelectionHit, Rect)>,
    selection_destination_hits: Vec<(String, Rect)>,
    /// Stable-id hit targets for the currently materialised grid cards.
    library_cards: Vec<(String, Rect, bool)>,
    pointer: Point,
    library_mode: bool,
}

struct DirectoryImport {
    receiver: Receiver<DirectoryImportEvent>,
    cancel: Arc<AtomicBool>,
    invocation: Option<tilo_actions::InvocationId>,
    discovered: usize,
    imported: usize,
    skipped: usize,
    done: bool,
}

enum DirectoryImportEvent {
    Candidate(std::path::PathBuf),
    Finished,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    fn drain_analysis_results(&mut self, doc: &mut AutoCommit) {
        while let Ok(completed) = self.analysis_rx.try_recv() {
            self.active_analysis.remove(&completed.invocation);
            self.active_people_scans.remove(&completed.invocation);
            if self.cancelled_analysis.remove(&completed.invocation) {
                continue;
            }
            let stale = matches!(&completed.result, AnalysisOutcome::Image(_))
                && PhotosDoc::photo(doc, &completed.photo_id)
                    .as_ref()
                    .is_none_or(|photo| photo.image.content != completed.content);
            let update = if stale {
                tilo_actions::InvocationUpdate::Failed {
                    invocation: completed.invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.analysis_stale".into(),
                        message:
                            "This photograph changed before local analysis finished. Retry it."
                                .into(),
                        details: None,
                        retryable: true,
                    },
                }
            } else {
                match completed.result {
                    AnalysisOutcome::Image(Ok(mut analysis)) => {
                        "shared".clone_into(&mut analysis.provenance.scope);
                        let action = crate::actions::PhotosAction::SetImageAnalysis {
                            photo_id: completed.photo_id,
                            analysis: Some(analysis),
                        };
                        match self.actions.try_dispatch_with_context(
                            doc,
                            crate::actions::PHOTOS_ACTION_TYPE,
                            &action,
                            tilo_actions::ActionContext::for_action(
                                "org.tilo.Photos",
                                "Photos",
                                &action,
                            ),
                        ) {
                            Ok(()) => {
                                self.notice = Some(
                                    "Local image analysis was shared with this photo library"
                                        .to_owned(),
                                );
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                            Err(error) => analysis_failure(completed.invocation, error.to_string()),
                        }
                    }
                    AnalysisOutcome::Image(Err(error)) => {
                        analysis_failure(completed.invocation, error)
                    }
                    AnalysisOutcome::People(Err(error)) => {
                        self.people_scan_progress.status = "error".to_owned();
                        self.people_scan_progress.last_error = Some(error.clone());
                        self.people_scan_progress.save();
                        analysis_failure(completed.invocation, error)
                    }
                    AnalysisOutcome::People(Ok(batch)) => {
                        let added = self.people_controller.ingest_verified_face_clusters(batch);
                        self.people_scan_progress.status = "paused".to_owned();
                        self.people_scan_progress.completed_batches = self
                            .people_scan_progress
                            .completed_batches
                            .saturating_add(1);
                        self.people_scan_progress.last_error = None;
                        self.people_scan_progress.save();
                        self.notice = Some(if added == 0 {
                            "No new local face suggestions were found in this batch".to_owned()
                        } else {
                            format!(
                                "{added} local People suggestion{} ready to review",
                                if added == 1 { " is" } else { "s are" }
                            )
                        });
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation: completed.invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    }
                }
            };
            self.resource_result = Some(update);
        }
    }

    fn toolbar_slots(&self) -> &'static [Slot] {
        if self.large_text_toolbar || (self.rect.width() > 0.0 && self.rect.width() < 600.0) {
            COMPACT_SLOTS
        } else {
            SLOTS
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "construction lists every app-owned state field explicitly so local, shared, and transient state remain auditable"
    )]
    pub fn new() -> Self {
        let (thumbnail_job_tx, thumbnail_job_rx) =
            std::sync::mpsc::sync_channel(THUMBNAIL_QUEUE_LIMIT);
        let (thumbnail_result_tx, thumbnail_result_rx) =
            std::sync::mpsc::sync_channel(THUMBNAIL_QUEUE_LIMIT);
        let (analysis_job_tx, analysis_job_rx) =
            std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        let (analysis_result_tx, analysis_rx) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        let analysis_tx = std::thread::Builder::new()
            .name("photos-local-analysis".to_owned())
            .spawn(move || analysis_worker(&analysis_job_rx, &analysis_result_tx))
            .ok()
            .map(|_| analysis_job_tx);
        let thumbnail_tx = std::thread::Builder::new()
            .name("photos-thumbnail".to_owned())
            .spawn(move || thumbnail_worker(&thumbnail_job_rx, &thumbnail_result_tx))
            .ok()
            .map(|_| thumbnail_job_tx);
        let pending: Rc<Cell<Option<Hit>>> = Rc::new(Cell::new(None));
        let album_page_pending: Rc<Cell<Option<AlbumEditorHit>>> = Rc::new(Cell::new(None));
        let timeline_density_pending: Rc<Cell<Option<TimelineHit>>> = Rc::new(Cell::new(None));
        let faces: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
        let mut buttons = Vec::new();
        for ((hit, key, icon, _), face) in TOOLBAR.iter().zip(faces.iter()) {
            buttons.push((*hit, build_button(*hit, key, *icon, *face, &pending)));
        }
        Self {
            actions: ui::ActionHost::new(),
            #[cfg(test)]
            last_library_action: None,
            original: None,
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
            compact_more_open: false,
            compact_more_focus: 0,
            compact_more_hits: Vec::new(),
            picker: None,
            directory_picker: None,
            directory_import: None,
            resource_invocation: None,
            resource_result: None,
            pending_resource_open: None,
            analysis_tx,
            analysis_rx,
            active_analysis: HashSet::new(),
            cancelled_analysis: HashSet::new(),
            active_people_scans: HashSet::new(),
            people_controller: crate::people::PeopleController::new(),
            people_scan_progress: PeopleScanProgress::load(),
            people_mode: false,
            canvas_state: CanvasState::Empty,
            search_provider: crate::search::PhotosSearchProvider::empty(),
            search_heads: Vec::new(),
            search_refresh_error: None,
            #[cfg(test)]
            force_search_refresh_error: false,
            library_items: Vec::new(),
            library_total: 0,
            library_search: SearchField::new()
                .with_id("photos-library-search")
                .with_placeholder("Search photos, people, places, text"),
            library_query: String::new(),
            library_destination: LibraryDestination::Timeline,
            places_list_mode: false,
            places_viewport: crate::places::MapViewport::default(),
            places_items: Vec::new(),
            places_hits: Vec::new(),
            places_map_bounds: Rect::ZERO,
            places_drag_origin: None,
            library_favorite_count: 0,
            library_offset: 0,
            library_albums: Vec::new(),
            active_album: None,
            active_folder: None,
            active_person: None,
            active_tag: None,
            active_object: None,
            active_pet: None,
            active_favorites: false,
            library_folders: Vec::new(),
            library_people: Vec::new(),
            library_person_covers: HashMap::new(),
            library_tags: Vec::new(),
            metadata_filter_cards: Vec::new(),
            library_page_hits: Vec::new(),
            timeline_density_hits: Vec::new(),
            timeline_density_buttons: build_timeline_density_buttons(&timeline_density_pending),
            timeline_density_pending,
            library_select_button: build_library_header_button("photos-library-select", "Select"),
            library_album_buttons: build_library_album_buttons(),
            timeline_density: 1,
            timeline_scroll_row: 0.0,
            timeline_last_columns: 1,
            timeline_index: DateIndexedScrollbar::new("photos:timeline-index"),
            timeline_index_markers: Vec::new(),
            timeline_index_metrics: ScrollMetrics::new(0.0, 0.0, 0.0),
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
            library_thumbnails: HashMap::new(),
            thumbnail_unavailable: HashSet::new(),
            original_decode_failed: None,
            thumbnail_tx,
            thumbnail_rx: thumbnail_result_rx,
            thumbnail_generation: Some(0),
            thumbnail_visible_ids: Vec::new(),
            thumbnail_queued: HashSet::new(),
            library_remove_confirm: None,
            selection_mode: false,
            selected_photo_ids: HashSet::new(),
            selection_anchor: None,
            selection_destination_album: None,
            selection_album_chooser: false,
            selection_album_page: 0,
            selection_all_favorite: false,
            selection_hits: Vec::new(),
            selection_destination_hits: Vec::new(),
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
    fn refit_toolbar(&mut self, width: f32) {
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
            faces_for_width(width)
        };
        if want == self.faces {
            return;
        }
        let enabled: Vec<bool> = self.buttons.iter().map(|(_, b)| b.is_enabled()).collect();
        self.buttons.clear();
        for (((hit, key, icon, _), face), was_enabled) in
            TOOLBAR.iter().zip(want.iter()).zip(enabled)
        {
            let mut button = build_button(*hit, key, *icon, *face, &self.pending);
            button.set_enabled(was_enabled);
            self.buttons.push((*hit, button));
        }
        self.faces = want;
    }

    /// Import `path` through the canonical durable action. The decoded source
    /// path remains only in this process; the action writes portable metadata
    /// and the staged content hash into the shared document.
    pub fn open_path(&mut self, doc: &mut AutoCommit, path: &str) {
        let bytes = bounded_file_bytes(path);
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
                Some((
                    *card,
                    details,
                    Rect::new(
                        card.x1 - LIBRARY_ACTION_H * 2.0,
                        card.y1 - LIBRARY_ACTION_H,
                        card.x1 - LIBRARY_ACTION_H,
                        card.y1,
                    ),
                    Rect::new(
                        card.x1 - LIBRARY_ACTION_H,
                        card.y1 - LIBRARY_ACTION_H,
                        card.x1,
                        card.y1,
                    ),
                ))
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

    /// Device-local People controller for a platform-specific face pipeline
    /// or the shared People management surface. It intentionally exposes no
    /// document reference: local clusters and their evidence never sync.
    pub fn people_controller(&self) -> &crate::people::PeopleController {
        &self.people_controller
    }

    /// Mutable local People controller. Callers use this for explicit opt-in,
    /// review, hiding, splitting, and deletion of private clusters.
    pub fn people_controller_mut(&mut self) -> &mut crate::people::PeopleController {
        &mut self.people_controller
    }

    /// Commit a reviewed cluster's compact, user-approved membership through
    /// the normal typed action dispatcher. The local cluster stays local; the
    /// action contains only the selected display name and stable photo ids.
    pub fn approve_people_cluster(&mut self, doc: &mut AutoCommit, cluster_id: &str) -> bool {
        let Some(action) = self.people_controller.approve_membership(cluster_id) else {
            self.notice = Some(
                "Name and confirm this local People suggestion before sharing its labels"
                    .to_owned(),
            );
            return false;
        };
        self.commit_library_action(doc, &action);
        self.notice =
            Some("Approved person labels were added to the shared photo library".to_owned());
        true
    }

    /// Reveal the bounded library grid.  Kept public for the deterministic
    /// offscreen gallery; ordinary interaction reaches it through the same
    /// toolbar button and semantic action as a user.
    pub fn show_library(&mut self) {
        self.library_mode = true;
        self.people_mode = false;
        self.library_destination = LibraryDestination::Timeline;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.library_remove_confirm = None;
        self.finish_selection();
    }

    /// Reveal the People collection without enabling recognition or changing
    /// any document metadata. This is used by the resource action and visual
    /// acceptance scenarios.
    pub fn show_people(&mut self) {
        self.show_library();
        self.people_mode = true;
        self.library_destination = LibraryDestination::People;
    }

    fn begin_selection(&mut self) {
        self.selection_mode = true;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_page = 0;
        self.selection_all_favorite = false;
        self.notice = Some("Select photographs, then apply a shared library action".to_owned());
    }

    fn finish_selection(&mut self) {
        self.selection_mode = false;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_page = 0;
        self.selection_all_favorite = false;
    }

    /// Toggle one materialised card, or the inclusive visible range from the
    /// previous anchor. A range is a view operation, so it is intentionally
    /// bounded to the cards the person can currently inspect.
    fn toggle_library_selection(&mut self, photo_id: &str, extend: bool) {
        if !self.selection_mode {
            return;
        }
        let visible: Vec<_> = self.library_cards.iter().map(|(id, _, _)| id).collect();
        if extend
            && let Some(anchor) = &self.selection_anchor
            && let (Some(start), Some(end)) = (
                visible.iter().position(|id| **id == *anchor),
                visible.iter().position(|id| **id == photo_id),
            )
        {
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            if let Some(range) = visible.get(start..=end) {
                for id in range {
                    self.selected_photo_ids.insert((**id).clone());
                }
            }
            return;
        }
        if !self.selected_photo_ids.insert(photo_id.to_owned()) {
            self.selected_photo_ids.remove(photo_id);
        }
        self.selection_anchor = Some(photo_id.to_owned());
    }

    fn select_all_visible(&mut self) {
        self.selected_photo_ids
            .extend(self.library_cards.iter().map(|(id, _, _)| id.clone()));
    }

    fn set_selected_favorite(&mut self, doc: &mut AutoCommit, favorite: bool) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some("Select at least one photograph first".to_owned());
            return;
        }
        self.commit_library_action(
            doc,
            &crate::actions::PhotosAction::SetFavorites {
                photo_ids: ids,
                favorite,
            },
        );
        self.notice = Some(if favorite {
            "Selected photographs added to Favorites".to_owned()
        } else {
            "Selected photographs removed from Favorites".to_owned()
        });
        self.finish_selection();
    }

    fn add_selected_to_destination_album(&mut self, doc: &mut AutoCommit) {
        let Some(album_id) = self.selection_destination_album.clone() else {
            self.notice = Some("Choose a destination album first".to_owned());
            return;
        };
        let library = PhotosDoc::library(doc);
        let Some(album) = library.album(&album_id) else {
            self.notice = Some("That album is no longer available".to_owned());
            return;
        };
        let mut photo_ids = album.photo_ids.clone();
        let mut selected: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        selected.sort_unstable();
        for id in selected {
            if !photo_ids.contains(&id) {
                photo_ids.push(id);
            }
        }
        if self.selected_photo_ids.is_empty() {
            self.notice = Some("Select at least one photograph first".to_owned());
            return;
        }
        self.commit_library_action(
            doc,
            &crate::actions::PhotosAction::SetAlbumMembership {
                album_id,
                photo_ids,
            },
        );
        self.notice = Some("Selected photographs added to the album".to_owned());
        self.finish_selection();
    }

    fn handle_selection_hit(&mut self, hit: SelectionHit, doc: &mut AutoCommit) {
        match hit {
            SelectionHit::Start => self.begin_selection(),
            SelectionHit::SelectAll => self.select_all_visible(),
            SelectionHit::Clear => {
                self.selected_photo_ids.clear();
                self.selection_anchor = None;
            }
            SelectionHit::Favorite => {
                let library = PhotosDoc::library(doc);
                let all_favorite = !self.selected_photo_ids.is_empty()
                    && self
                        .selected_photo_ids
                        .iter()
                        .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
                self.set_selected_favorite(doc, !all_favorite);
            }
            SelectionHit::ChooseAlbum => {
                if self.selected_photo_ids.is_empty() || self.library_albums.is_empty() {
                    self.notice = Some("Select photographs and create an album first".to_owned());
                } else {
                    self.selection_album_chooser = true;
                    self.selection_album_page = 0;
                }
            }
            SelectionHit::CancelDestinationChooser => {
                self.selection_album_chooser = false;
            }
            SelectionHit::ApplyDestination => {
                if self.selection_destination_album.is_some() {
                    self.selection_album_chooser = false;
                } else {
                    self.notice = Some("Choose a destination album first".to_owned());
                }
            }
            SelectionHit::PreviousDestinationPage => {
                self.selection_album_page = self.selection_album_page.saturating_sub(1);
            }
            SelectionHit::NextDestinationPage => {
                let page_count = self
                    .library_albums
                    .len()
                    .div_ceil(SELECTION_ALBUMS_PER_PAGE);
                self.selection_album_page =
                    (self.selection_album_page + 1).min(page_count.saturating_sub(1));
            }
            SelectionHit::AddToAlbum => self.add_selected_to_destination_album(doc),
            SelectionHit::Done => self.finish_selection(),
        }
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

    /// Pose the offline Places destination for deterministic visual coverage.
    pub fn show_places_for_shot(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_viewport = crate::places::MapViewport {
            latitude: 37.0,
            longitude: -40.0,
            zoom: 1.5,
        };
    }

    /// Pose the ordinary selection shelf for deterministic visual coverage.
    /// Two real photographs are selected so the capture exercises the active
    /// controls and selected-card treatment, not merely their disabled state.
    pub fn show_selection_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        self.begin_selection();
        for photo in PhotosDoc::library(doc).photos.into_iter().take(2) {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id);
        }
    }

    /// Pose the first page of a deliberately long destination list. The
    /// selection is local; this exists only to make pagination visible in the
    /// deterministic accessibility and responsive screenshots.
    pub fn show_selection_destination_chooser_for_shot(&mut self, doc: &AutoCommit) {
        if let Some(photo) = PhotosDoc::library(doc).photos.first() {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id.clone());
        }
        self.selection_album_chooser = true;
        self.selection_album_page = 0;
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
        if let Ok(()) = atomic_file::write(std::path::Path::new(path), &bytes) {
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
            tilo_actions::ActionContext::for_action(
                "org.tilo.Photos",
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
        #[cfg(test)]
        {
            self.last_library_action =
                tilo_actions::EncodedAction::encode(crate::actions::PHOTOS_ACTION_TYPE, action)
                    .ok();
        }
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("org.tilo.Photos", "Photos", &action),
        );
    }

    fn begin_new_album(&mut self, _doc: &AutoCommit) {
        // A local `untitled-1` counter collides when two offline replicas
        // create their first album. Mint the durable identity independently
        // of its editable name so concurrent creations remain separate.
        let id = format!("photos.album:{}", tilo_actions::InvocationId::new().0);
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
    fn library_page_step(&self) -> usize {
        if self.large_text_toolbar && self.canvas_rect.width() < 620.0 {
            if self.album_editor { 2 } else { 1 }
        } else {
            LIBRARY_PAGE_SIZE
        }
    }

    fn page_library(&mut self, forward: bool) {
        let step = self.library_page_step();
        if forward {
            self.library_offset =
                (self.library_offset + step).min(self.library_total.saturating_sub(1));
        } else {
            self.library_offset = self.library_offset.saturating_sub(step);
        }
    }

    fn can_page_library(&self, forward: bool) -> bool {
        let step = self.library_page_step();
        if forward {
            self.library_offset.saturating_add(step) < self.library_total
        } else {
            self.library_offset > 0
        }
    }

    fn library_page_label(&self) -> String {
        if self.library_total == 0 {
            "Showing 0 of 0".to_owned()
        } else {
            let first = self.library_offset + 1;
            let last = (self.library_offset + self.library_page_step()).min(self.library_total);
            format!("Showing {first}–{last} of {}", self.library_total)
        }
    }

    /// Change the local timeline density through any input source.  One step
    /// is shared by the +/- affordances, Ctrl-wheel and pinch, which makes
    /// their resulting layout testable for parity.
    fn adjust_timeline_density(&mut self, increase: bool) -> bool {
        let before = self.timeline_density;
        let anchor = (self.timeline_scroll_row * self.timeline_last_columns.max(1) as f64)
            .round()
            .max(0.0);
        if increase {
            self.timeline_density = self.timeline_density.saturating_add(1).min(4);
        } else {
            self.timeline_density = self.timeline_density.saturating_sub(1);
        }
        if before == self.timeline_density {
            return false;
        }
        let columns = timeline_column_count(
            self.canvas_rect.width(),
            self.timeline_density,
            self.large_text_toolbar,
        );
        self.timeline_scroll_row = anchor / columns.max(1) as f64;
        self.timeline_last_columns = columns;
        true
    }

    fn scroll_timeline_rows(&mut self, delta: f64) {
        self.timeline_scroll_row = (self.timeline_scroll_row + delta).max(0.0);
    }

    /// Apply every chronological-index input through the same local scroll
    /// and density helpers used by the ordinary timeline.  The index never
    /// owns a second offset, which is what keeps a pinch anchor and a dragged
    /// date thumb in agreement.
    fn apply_timeline_index_intent(&mut self, intent: DateIndexIntent) -> bool {
        match intent {
            DateIndexIntent::ScrollTo { position } => {
                self.timeline_scroll_row =
                    DateIndexedScrollbar::offset_for(self.timeline_index_metrics, position);
                true
            }
            DateIndexIntent::AdjustDensity { delta } => {
                if delta > 0 {
                    self.adjust_timeline_density(true)
                } else if delta < 0 {
                    self.adjust_timeline_density(false)
                } else {
                    false
                }
            }
            // A preview is presentation only. The widget has retained it for
            // paint and AT's current value; it must not make collaboration
            // history noisy by changing a collection just because it was
            // hovered.
            DateIndexIntent::PreviewChanged(_) => true,
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
        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
        ] {
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
            crate::actions::PhotosOperation::ImportDirectory { path: None, .. } => {
                self.directory_picker = Some(
                    ui::portal::PickRequest::directory("Import photo folder", "Import folder")
                        .thread_name("photos-portal-directory-import")
                        .send(),
                );
            }
            crate::actions::PhotosOperation::ImportDirectory {
                path: Some(path), ..
            } => {
                self.begin_directory_import(std::path::PathBuf::from(path), None);
            }
            crate::actions::PhotosOperation::ImportCameraMedia { media_id, name } => {
                return Err(format!(
                    "Camera import requires document dispatch: {media_id} ({name})"
                ));
            }
            crate::actions::PhotosOperation::ReindexDirectory { .. } => {
                return Err("directory indexing requires the Photos indexing job host".to_owned());
            }
            crate::actions::PhotosOperation::AnalyzeText { .. }
            | crate::actions::PhotosOperation::AnalyzeImage { .. }
            | crate::actions::PhotosOperation::AnalyzePeopleLibrary => {
                return Err("local image analysis requires document dispatch".to_owned());
            }
            crate::actions::PhotosOperation::Copy => {
                self.copy_to_clipboard();
            }
            crate::actions::PhotosOperation::ShareReference { resource_id } => {
                let exists = PhotosDoc::image(doc)
                    .is_some_and(|image| crate::actions::resource_id(&image) == resource_id)
                    || PhotosDoc::photo(doc, &resource_id).is_some();
                if exists {
                    // This is deliberately a local clipboard handoff, not an
                    // invented upload or recipient-delivery success. The
                    // address identifies either the open editor image or an
                    // unselected, addressable library card.
                    ui::clipboard_copy(&format!("tilo://resource/{resource_id}"));
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
            crate::actions::PhotosOperation::View => {
                self.library_mode = false;
                self.album_editor = false;
                self.metadata_editor = false;
            }
            crate::actions::PhotosOperation::ViewLibrary => self.show_library(),
            crate::actions::PhotosOperation::ViewPeople => {
                self.show_library();
                self.people_mode = true;
                self.library_destination = LibraryDestination::People;
                self.notice = Some(
                    "People is local-first: install a signed face model, then review every suggestion before sharing labels".to_owned(),
                );
            }
            crate::actions::PhotosOperation::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Settings › Models to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::EnablePeopleRecognition => {
                self.people_controller.set_recognition_enabled(true);
                self.people_controller
                    .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                self.notice = Some(self.people_controller.capability().summary().to_owned());
            }
            crate::actions::PhotosOperation::ViewFavorites => self.view_favorites(doc)?,
            crate::actions::PhotosOperation::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.notice = Some("Showing offline Places".to_owned());
            }
            crate::actions::PhotosOperation::ViewPlace { place_key } => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                if let Some(photo) = PhotosDoc::library(doc).photos.iter().find(|photo| {
                    photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == place_key)
                }) && let Some(location) = &photo.location
                {
                    self.places_viewport.latitude = location.latitude;
                    self.places_viewport.longitude = location.longitude;
                    self.places_viewport.zoom = 12.0;
                }
                self.notice = Some("Showing selected place offline".to_owned());
            }
            crate::actions::PhotosOperation::ViewAlbum { .. }
            | crate::actions::PhotosOperation::ViewFolder { .. }
            | crate::actions::PhotosOperation::ViewPerson { .. }
            | crate::actions::PhotosOperation::ViewTag { .. }
            | crate::actions::PhotosOperation::ViewObject { .. }
            | crate::actions::PhotosOperation::ViewPet { .. }
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
        if PhotosDoc::album(doc, album_id).is_none() {
            return Err("that album is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = Some(album_id.to_owned());
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
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
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = false;
        self.library_offset = 0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing shared photo metadata view".to_owned());
        Ok(())
    }

    /// Open one exact intelligence-derived collection. These labels are
    /// still local navigation; only the accepted analysis record is shared.
    fn view_analysis_filter(
        &mut self,
        doc: &AutoCommit,
        label: String,
        pet: bool,
    ) -> Result<(), String> {
        let exists = PhotosDoc::library(doc).photos.iter().any(|photo| {
            photo.analysis.as_ref().is_some_and(|analysis| {
                if pet {
                    analysis.pets.contains(&label)
                } else {
                    analysis.objects.contains(&label)
                }
            })
        });
        if !exists {
            return Err("that analysis collection is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = (!pet).then_some(label.clone());
        self.active_pet = pet.then_some(label);
        self.active_favorites = false;
        self.library_offset = 0;
        self.album_editor = false;
        self.notice = Some("Showing explicitly shared image analysis".to_owned());
        Ok(())
    }

    /// Navigate to the stable favorites resource after verifying that it is
    /// still non-empty. A stale global-search result must fail rather than
    /// silently changing the user's view to an unrelated all-photos shelf.
    fn view_favorites(&mut self, doc: &AutoCommit) -> Result<(), String> {
        if !PhotosDoc::library(doc)
            .photos
            .iter()
            .any(|photo| photo.favorite)
        {
            return Err("there are no favorite photographs in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = None;
        self.active_folder = None;
        self.active_person = None;
        self.active_tag = None;
        self.active_object = None;
        self.active_pet = None;
        self.active_favorites = true;
        self.library_offset = 0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing favorite photographs".to_owned());
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
        if self.original.as_ref().map(|(p, _)| p.as_str()) != Some(open.path.as_str()) {
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
                self.original = Some((open.path.clone(), img));
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
            .is_some_and(|r| r.path == open.path && r.edits == edits);
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
            path: open.path,
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
                let _ = self.perform_effect(crate::actions::PhotosOperation::ViewLibrary, doc);
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
            let mut cards = Vec::new();
            if self.library_destination == LibraryDestination::Places {
                cards.extend(self.places_hits.iter().map(|(hit, bounds)| {
                    let (name, target) = match hit {
                        PlacesHit::Timeline => ("Timeline", "photos:places:timeline".to_owned()),
                        PlacesHit::People => ("People", "photos:places:people".to_owned()),
                        PlacesHit::Places => ("Places", "photos:places:places".to_owned()),
                        PlacesHit::ToggleList => (
                            if self.places_list_mode {
                                "Show map"
                            } else {
                                "Show list"
                            },
                            "photos:places:toggle-list".to_owned(),
                        ),
                        PlacesHit::ZoomIn => ("Zoom in", "photos:places:zoom-in".to_owned()),
                        PlacesHit::ZoomOut => ("Zoom out", "photos:places:zoom-out".to_owned()),
                        PlacesHit::Marker(id) => (
                            "Open photograph at this place",
                            format!("photos:places:photo:{id}"),
                        ),
                    };
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                        .with_action_target(target)
                }));
                AccessNode::new(Role::Group)
                    .with_name("Places")
                    .with_description("Offline map of explicitly shared photo locations. Arrow keys pan; plus and minus zoom; L changes to the list.")
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:places")
                    .with_children(cards)
            } else {
                cards.extend(
                    self.library_cards
                        .iter()
                        .filter_map(|(id, bounds, favorite)| {
                            let item = self.library_items.iter().find(|item| item.id == *id)?;
                            let name = &item.name;
                            let open_target = format!("photos:library-photo:{id}");
                            let mut children = vec![if self.selection_mode {
                                AccessNode::new(Role::ToggleButton)
                                .with_name(name.clone())
                                .with_description(
                                    "Selected photographs receive the chosen shared batch action",
                                )
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    checked: Some(self.selected_photo_ids.contains(id)),
                                    ..Default::default()
                                })
                                .with_action_target(open_target)
                            } else if self.album_editor {
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
                            if !self.album_editor && !self.selection_mode {
                                let favorite_target = format!("photos:library-favorite:{id}");
                                let favorite_bounds = Rect::new(
                                    bounds.x1 - 44.0,
                                    bounds.y1 - 44.0,
                                    bounds.x1,
                                    bounds.y1,
                                );
                                children.push(
                                    AccessNode::new(Role::ToggleButton)
                                        .with_name("Favorite")
                                        .with_bounds(favorite_bounds)
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
                                        .with_description(
                                            "Removes this photograph from every album",
                                        )
                                        .with_bounds(Rect::new(
                                            bounds.x1 - 88.0,
                                            bounds.y1 - 44.0,
                                            bounds.x1 - 44.0,
                                            bounds.y1,
                                        ))
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
                                        .with_bounds(Rect::new(
                                            bounds.x0 + 8.0,
                                            bounds.y1 - 44.0,
                                            (bounds.x0 + 108.0).min(bounds.x1 - 88.0),
                                            bounds.y1,
                                        ))
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
                        }),
                );
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
                cards.extend(self.metadata_filter_cards.iter().map(|card| {
                    let label = if card.favorites {
                        "Favorites".to_owned()
                    } else {
                        match (&card.folder, &card.person, &card.tag) {
                            (Some(folder), None, None) => format!("Folder: {folder}"),
                            (None, Some(person), None) => format!("People: {person}"),
                            (None, None, Some(tag)) => format!("Tag: {tag}"),
                            _ => "All metadata".to_owned(),
                        }
                    };
                    AccessNode::new(Role::ToggleButton)
                        .with_name(label)
                        .with_bounds(card.bounds)
                        .with_state(AccessState {
                            focusable: true,
                            checked: Some(
                                card.folder.as_ref() == self.active_folder.as_ref()
                                    && card.person.as_ref() == self.active_person.as_ref()
                                    && card.tag.as_ref() == self.active_tag.as_ref()
                                    && card.favorites == self.active_favorites
                                    && self.active_album.is_none(),
                            ),
                            ..Default::default()
                        })
                        .with_action_target(if card.favorites {
                            "photos:library-favorites".to_owned()
                        } else {
                            format!(
                                "photos:library-metadata:{}",
                                metadata_filter_id(
                                    card.folder.as_deref(),
                                    card.person.as_deref(),
                                    card.tag.as_deref()
                                )
                            )
                        })
                }));
                for (hit, bounds) in &self.timeline_density_hits {
                    let (name, target) = match hit {
                        TimelineHit::ZoomOut => {
                            ("Show fewer photographs", "photos:timeline:density-out")
                        }
                        TimelineHit::ZoomIn => {
                            ("Show more photographs", "photos:timeline:density-in")
                        }
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_description(
                                "Changes how many photographs appear in each timeline row",
                            )
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                for (forward, enabled, bounds) in &self.library_page_hits {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(if *forward {
                                "Show next photograph"
                            } else {
                                "Show previous photograph"
                            })
                            .with_description(self.library_page_label())
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: *enabled,
                                disabled: !*enabled,
                                ..Default::default()
                            })
                            .with_action_target(if *forward {
                                "photos:library-page:next"
                            } else {
                                "photos:library-page:previous"
                            }),
                    );
                }
                if self.selection_album_chooser {
                    // A modal needs one focus scope. Leaving the dimmed grid,
                    // filters, or card operations in the AT tree would let a
                    // screen-reader user activate controls a pointer user cannot.
                    cards.clear();
                }
                for (hit, bounds) in &self.selection_hits {
                    let (name, target, disabled) = match hit {
                        SelectionHit::Start => {
                            ("Select photographs", "photos:selection:start", false)
                        }
                        SelectionHit::SelectAll => (
                            "Select all visible photographs",
                            "photos:selection:all",
                            false,
                        ),
                        SelectionHit::Clear => (
                            "Clear photograph selection",
                            "photos:selection:clear",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::Favorite => (
                            if self.selection_all_favorite {
                                "Remove selected photographs from Favorites"
                            } else {
                                "Add selected photographs to Favorites"
                            },
                            "photos:selection:favorite",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::ChooseAlbum => (
                            "Choose destination album",
                            "photos:selection:choose-album",
                            self.selected_photo_ids.is_empty() || self.library_albums.is_empty(),
                        ),
                        SelectionHit::CancelDestinationChooser => (
                            "Cancel destination choice",
                            "photos:selection:destination-cancel",
                            false,
                        ),
                        SelectionHit::ApplyDestination => (
                            "Use selected destination album",
                            "photos:selection:destination-apply",
                            self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::PreviousDestinationPage => (
                            "Show earlier destination albums",
                            "photos:selection:destination-previous",
                            self.selection_album_page == 0,
                        ),
                        SelectionHit::NextDestinationPage => (
                            "Show more destination albums",
                            "photos:selection:destination-next",
                            (self.selection_album_page + 1) * SELECTION_ALBUMS_PER_PAGE
                                >= self.library_albums.len(),
                        ),
                        SelectionHit::AddToAlbum => (
                            "Add selected photographs to selected album",
                            "photos:selection:add-to-album",
                            self.selected_photo_ids.is_empty()
                                || self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::Done => (
                            "Finish selecting photographs",
                            "photos:selection:done",
                            false,
                        ),
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: !disabled,
                                disabled,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                for (album_id, bounds) in &self.selection_destination_hits {
                    let name = self
                        .library_albums
                        .iter()
                        .find(|(id, _, _)| id == album_id)
                        .map_or_else(
                            || "Choose destination album".to_owned(),
                            |(_, name, _)| format!("Use {name} as destination album"),
                        );
                    cards.push(
                        AccessNode::new(Role::ToggleButton)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                checked: Some(
                                    self.selection_destination_album.as_deref()
                                        == Some(album_id.as_str()),
                                ),
                                ..Default::default()
                            })
                            .with_action_target(format!("photos:selection:destination:{album_id}")),
                    );
                }
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
                            MetadataEditorHit::Save => {
                                ("Save photo details", "photos:metadata:save")
                            }
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
                            AlbumEditorHit::Delete => {
                                ("Delete album", "photos:album-editor:delete")
                            }
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
                            AlbumEditorHit::Edit => {
                                ("Edit selected album", "photos:album-editor:edit")
                            }
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
                if self.selection_album_chooser {
                    AccessNode::new(Role::Dialog)
                        .with_name("Choose destination album")
                        .with_description("Choose one album, then use it as the destination")
                        .with_bounds(self.canvas_rect)
                        .with_state(AccessState::new().modal(true))
                        .with_hashed_key("photos:destination-album-dialog")
                        .with_children(cards)
                } else {
                    if !self.album_editor && !self.metadata_editor {
                        cards.push(self.timeline_index.accessibility(
                            self.timeline_index_metrics,
                            &self.timeline_index_markers,
                        ));
                    }
                    AccessNode::new(Role::List)
                        .with_name(tr("photos-library"))
                        .with_description(format!("{} photographs", self.library_total))
                        .with_bounds(self.canvas_rect)
                        .with_hashed_key("photos:library-grid")
                        .with_children(cards)
                }
            }
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
        if target.starts_with("photos:timeline-index:") {
            if matches!(action, AccessAction::Focus) {
                self.timeline_index.set_focus(true);
                return true;
            }
            if let Some(intent) = self.timeline_index.perform_access_action(
                target,
                action,
                self.timeline_index_metrics,
                &self.timeline_index_markers,
            ) {
                return self.apply_timeline_index_intent(intent);
            }
            return false;
        }
        if let Some(direction) = target.strip_prefix("photos:timeline:density-") {
            return match (direction, action) {
                ("in", AccessAction::Activate) => self.adjust_timeline_density(true),
                ("out", AccessAction::Activate) => self.adjust_timeline_density(false),
                ("in" | "out", AccessAction::Focus) => true,
                _ => false,
            };
        }
        if let Some(command) = target.strip_prefix("photos:selection:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if let Some(album_id) = command.strip_prefix("destination:") {
                    if PhotosDoc::album(doc, album_id).is_none() {
                        self.notice =
                            Some("That destination album is no longer available".to_owned());
                        return false;
                    }
                    self.selection_destination_album = Some(album_id.to_owned());
                    self.notice =
                        Some("Destination album selected; choose Use album to continue".to_owned());
                    return true;
                }
                let hit = match command {
                    "start" => SelectionHit::Start,
                    "all" => SelectionHit::SelectAll,
                    "clear" => SelectionHit::Clear,
                    "favorite" => SelectionHit::Favorite,
                    "choose-album" => SelectionHit::ChooseAlbum,
                    "destination-cancel" => SelectionHit::CancelDestinationChooser,
                    "destination-apply" => SelectionHit::ApplyDestination,
                    "destination-previous" => SelectionHit::PreviousDestinationPage,
                    "destination-next" => SelectionHit::NextDestinationPage,
                    "add-to-album" => SelectionHit::AddToAlbum,
                    "done" => SelectionHit::Done,
                    _ => return false,
                };
                self.handle_selection_hit(hit, doc);
            }
            return true;
        }
        if let Some(direction) = target.strip_prefix("photos:library-page:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let forward = match direction {
                "next" => true,
                "previous" => false,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) && self.can_page_library(forward) {
                self.page_library(forward);
            }
            return true;
        }
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
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            self.active_album = (album_id != "all").then(|| album_id.to_owned());
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
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
        if target == "photos:library-favorites" {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                let _ = self.view_favorites(doc);
            }
            return true;
        }
        if let Some(filter_id) = target.strip_prefix("photos:library-metadata:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            let Some(card) = self.metadata_filter_cards.iter().find(|card| {
                metadata_filter_id(
                    card.folder.as_deref(),
                    card.person.as_deref(),
                    card.tag.as_deref(),
                ) == filter_id
            }) else {
                return false;
            };
            self.active_album = None;
            self.active_folder.clone_from(&card.folder);
            self.active_person.clone_from(&card.person);
            self.active_tag.clone_from(&card.tag);
            self.active_favorites = card.favorites;
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
        if self.selection_album_chooser {
            return false;
        }
        if !matches!(action, AccessAction::Activate) {
            if matches!(action, AccessAction::Focus) && self.selection_mode {
                self.selection_anchor = Some(photo_id.to_owned());
            }
            return matches!(action, AccessAction::Focus);
        }
        let Some(item) = self.library_items.iter().find(|item| item.id == photo_id) else {
            return false;
        };
        let favorite = item.favorite;
        if self.selection_mode {
            self.toggle_library_selection(photo_id, false);
        } else if remove {
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

    fn begin_directory_import(
        &mut self,
        path: std::path::PathBuf,
        invocation: Option<tilo_actions::InvocationId>,
    ) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(DIRECTORY_IMPORT_QUEUE);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let _ = std::thread::Builder::new()
            .name("photos-directory-import".into())
            .spawn(move || {
                let mut pending = vec![path];
                let mut seen = 0usize;
                while let Some(directory) = pending.pop() {
                    if worker_cancel.load(Ordering::Relaxed) || seen >= DIRECTORY_IMPORT_MAX_FILES {
                        break;
                    }
                    let Ok(entries) = std::fs::read_dir(directory) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        if worker_cancel.load(Ordering::Relaxed)
                            || seen >= DIRECTORY_IMPORT_MAX_FILES
                        {
                            break;
                        }
                        let path = entry.path();
                        if path.is_dir() {
                            pending.push(path);
                            continue;
                        }
                        if !path.is_file() || !is_importable_photo(&path) {
                            continue;
                        }
                        seen = seen.saturating_add(1);
                        if sender.send(DirectoryImportEvent::Candidate(path)).is_err() {
                            return;
                        }
                    }
                }
                let _ = sender.send(DirectoryImportEvent::Finished);
            });
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel,
            invocation,
            discovered: 0,
            imported: 0,
            skipped: 0,
            done: false,
        });
        self.notice = Some("Preparing folder import…".into());
    }

    fn drain_directory_import(&mut self, doc: &mut AutoCommit) {
        if self.directory_import.is_none() {
            return;
        }
        for _ in 0..DIRECTORY_IMPORT_PER_FRAME {
            let event = self
                .directory_import
                .as_ref()
                .map(|import| import.receiver.try_recv());
            match event {
                None => return,
                Some(Ok(DirectoryImportEvent::Candidate(path))) => {
                    if let Some(import) = self.directory_import.as_mut() {
                        import.discovered = import.discovered.saturating_add(1);
                    }
                    let before = PhotosDoc::library(doc).photos.len();
                    self.open_path(doc, &path.to_string_lossy());
                    let imported = PhotosDoc::library(doc).photos.len() > before;
                    if let Some(import) = self.directory_import.as_mut() {
                        if imported {
                            import.imported = import.imported.saturating_add(1);
                        } else {
                            import.skipped = import.skipped.saturating_add(1);
                        }
                    }
                }
                Some(Ok(DirectoryImportEvent::Finished)) => {
                    if let Some(import) = self.directory_import.as_mut() {
                        import.done = true;
                    }
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => break,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    if let Some(import) = self.directory_import.as_mut() {
                        import.done = true;
                    }
                }
            }
        }
        let Some(import) = self.directory_import.as_ref() else {
            return;
        };
        if import.done {
            let message = format!(
                "Folder import finished: {} imported, {} skipped",
                import.imported, import.skipped
            );
            let invocation = import.invocation;
            self.notice = Some(message.clone());
            self.directory_import = None;
            if let Some(invocation) = invocation {
                self.resource_result = Some(tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                });
            }
        } else {
            self.notice = Some(format!(
                "Importing folder: {} found · {} imported · {} skipped",
                import.discovered, import.imported, import.skipped
            ));
        }
    }

    /// Collect a chosen file from the portal, if one has arrived.
    fn drain_picker(&mut self, doc: &mut AutoCommit) {
        if let Some(rx) = self.directory_picker.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(path)) => {
                    self.directory_picker = None;
                    let invocation = self.resource_invocation.take();
                    self.begin_directory_import(path, invocation);
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => {
                    self.directory_picker = None;
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some("folder selection was cancelled".into()),
                        });
                    }
                }
                Ok(ui::portal::PortalOutcome::Failed(message)) => {
                    self.directory_picker = None;
                    self.notice = Some(format!("Folder selection failed: {message}"));
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_portal_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.directory_picker = None;
                }
            }
        }
        let Some(rx) = self.picker.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.picker = None;
                let path = path.to_string_lossy().into_owned();
                self.open_path(doc, &path);
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(if self.open_image_path().is_some() {
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    } else {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
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
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("photo selection was cancelled".into()),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.picker = None;
                self.notice = Some("The system file picker stopped unexpectedly".into());
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_disconnected".into(),
                            message: "the system file picker stopped unexpectedly".into(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
            }
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
            return self
                .notice
                .iter()
                .chain(self.search_refresh_error.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("  ·  ");
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
        if let Some(error) = &self.search_refresh_error {
            s.push_str("  ·  ");
            s.push_str(error);
        }
        s
    }

    fn refresh_search_provider(
        &self,
        doc: &AutoCommit,
    ) -> Result<(), ui::search::SearchCatalogError> {
        #[cfg(test)]
        if self.force_search_refresh_error {
            return Err(ui::search::SearchCatalogError::NonCanonicalResult);
        }
        self.search_provider.sync(doc)
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

    /// The shared empty-state composition is ideal at normal reading sizes,
    /// but its single centred title/body runs cannot reflow inside a 390 px
    /// viewport once text reaches 200%. Keep that exceptional presentation
    /// local while retaining the same copy, icon, colors and semantics.
    fn draw_empty_state(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
        body: &str,
    ) {
        if theme.text_scale <= 1.25 || self.canvas_rect.width() >= 620.0 {
            EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.canvas_rect,
                Some(Icon::Image),
                title,
                Some(body),
                None,
                Point::ZERO,
            );
            return;
        }

        let bounds = self.canvas_rect;
        let content_w = (bounds.width() - 48.0).max(120.0);
        let title_size = 16.0;
        let body_size = 11.0;
        let title_lines = wrap_words(title, 19);
        let body_lines = wrap_words(body, 28);
        let scale = f64::from(theme.text_scale);
        let title_step = 21.0 * scale;
        let body_step = 15.0 * scale;
        let icon_r = 29.0;
        let content_h = icon_r * 2.0
            + 26.0
            + title_step * title_lines.len() as f64
            + 12.0
            + body_step * body_lines.len() as f64;
        let top = bounds.y0 + ((bounds.height() - content_h) * 0.42).max(24.0);
        let icon_center = Point::new(bounds.center().x, top + icon_r);
        let icon_plate = Circle::new(icon_center, icon_r);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &icon_plate,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &icon_plate,
        );
        draw_icon(
            scene,
            transform,
            Rect::from_center_size(icon_center, (24.0, 24.0)),
            Icon::Image,
            theme.tokens.colors.accent[6],
            1.5,
        );

        let mut y = top + icon_r * 2.0 + 26.0;
        for line in title_lines {
            draw_centered_text(
                scene,
                transform,
                text_renderer,
                theme.text_primary,
                bounds.center().x,
                y,
                content_w,
                title_size,
                TextWeight::Medium,
                &line,
            );
            y += title_step;
        }
        y += 12.0;
        for line in body_lines {
            draw_centered_text(
                scene,
                transform,
                text_renderer,
                theme.text_dim,
                bounds.center().x,
                y,
                content_w,
                body_size,
                TextWeight::Regular,
                &line,
            );
            y += body_step;
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
                            height: Dimension::length(BTN_H * 0.6),
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
                        height: Dimension::length(BAR_H),
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
    fn build_status_node(&self, taffy: &mut TaffyTree<()>, theme: &Theme) -> NodeId {
        taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(if self.status.is_empty() {
                        0.0
                    } else {
                        (BAR_H * 0.8 * theme.text_scale).max(BAR_H * 0.6)
                    }),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap()
    }

    #[allow(clippy::unwrap_used, reason = "fresh layout node with fixed size")]
    fn build_library_search_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.library_search
            .set_placeholder(if theme.text_scale > 1.25 {
                "Search photos"
            } else {
                "Search photos, people, places, text"
            });
        let field = self.library_search.build_layout(taffy, theme, node_map);
        taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(if self.library_mode { 46.0 } else { 0.0 }),
                    },
                    padding: taffy::Rect {
                        left: LengthPercentage::length(PAD),
                        right: LengthPercentage::length(PAD),
                        top: LengthPercentage::length(if self.library_mode { 4.0 } else { 0.0 }),
                        bottom: LengthPercentage::length(if self.library_mode { 4.0 } else { 0.0 }),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &[field],
            )
            .unwrap()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the grid paint keeps its pixel geometry and bounded materialisation together"
    )]
    /// Render the visible part of the library; the remainder stays reachable
    /// through cursor-paged search and canonical resources.
    /// A deliberately calm People home: approved shared identities are a
    /// collection, while local face work is a private review queue. It never
    /// paints a made-up suggestion when no signed face model is installed.
    fn draw_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use vello::peniko::Fill;
        // This distinct collection must not leave stale library-card hit
        // targets alive behind it when a person opens People from a timeline.
        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        let bounds = self.canvas_rect;
        let inset = f64::from(PAD) + 18.0;
        let content = Rect::new(
            bounds.x0 + inset,
            bounds.y0 + 18.0,
            bounds.x1 - inset,
            bounds.y1 - 18.0,
        );
        text_renderer.draw_text_weighted(
            scene,
            "People",
            28.0,
            theme.text_primary,
            transform * Affine::translate((content.x0, content.y0)),
            Some(content.width() as f32),
            TextWeight::Semibold,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Private recognition stays on this device. Only names you approve are shared.",
            14.0,
            theme.text_dim,
            transform * Affine::translate((content.x0, content.y0 + 37.0)),
            Some(content.width() as f32),
            TextWeight::Regular,
        );
        let capability = self.people_controller.capability().summary();
        let privacy = if self.people_controller.privacy().recognition_enabled {
            "On for this device"
        } else {
            "Off for this device"
        };
        let notice = Rect::new(
            content.x0,
            content.y0 + 66.0,
            content.x1,
            content.y0 + 126.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &notice,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(notice.x0, notice.y0, notice.x1, notice.y0 + 1.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            privacy,
            14.0,
            theme.text_primary,
            transform * Affine::translate((notice.x0 + 16.0, notice.y0 + 13.0)),
            Some(220.0),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            capability,
            13.0,
            theme.text_dim,
            transform * Affine::translate((notice.x0 + 16.0, notice.y0 + 34.0)),
            Some((notice.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        // `library_people` is the bounded projection refreshed from the same
        // document as the canonical resource tree; draw never materializes a
        // large CRDT collection merely to paint this overview.
        let mut y = notice.y1 + 30.0;
        text_renderer.draw_text_weighted(
            scene,
            "Approved people",
            17.0,
            theme.text_primary,
            transform * Affine::translate((content.x0, y)),
            Some(content.width() as f32),
            TextWeight::Medium,
        );
        y += 28.0;
        let people: Vec<_> = self.library_people.iter().take(8).collect();
        if people.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                "No approved names yet. Review a local suggestion before sharing it.",
                14.0,
                theme.text_dim,
                transform * Affine::translate((content.x0, y)),
                Some(content.width() as f32),
                TextWeight::Regular,
            );
        } else {
            for (index, (name, count)) in people.into_iter().enumerate() {
                let columns = if content.width() < 620.0 { 2 } else { 4 };
                let gap = 12.0;
                let card_w = (content.width() - gap * (columns - 1) as f64) / columns as f64;
                let x = content.x0 + (index % columns) as f64 * (card_w + gap);
                let card_y = y + (index / columns) as f64 * 184.0;
                let card = Rect::new(x, card_y, x + card_w, card_y + 170.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.window_card,
                    None,
                    &card,
                );
                let preview = Rect::new(card.x0, card.y0, card.x1, card.y0 + 112.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.neutral[2],
                    None,
                    &preview,
                );
                if let Some(id) = self.library_person_covers.get(name)
                    && let Some(image) = self.library_thumbnails.get(id)
                {
                    let scale = (preview.width() / f64::from(image.width))
                        .max(preview.height() / f64::from(image.height));
                    let image_transform = transform
                        * Affine::translate((
                            preview.x0 + (preview.width() - f64::from(image.width) * scale) * 0.5,
                            preview.y0 + (preview.height() - f64::from(image.height) * scale) * 0.5,
                        ))
                        * Affine::scale(scale);
                    scene.draw_image(image, image_transform);
                } else {
                    let avatar = Rect::from_center_size(preview.center(), (38.0, 38.0));
                    draw_icon(scene, transform, avatar, Icon::Image, theme.text_dim, 1.5);
                }
                text_renderer.draw_text_weighted(
                    scene,
                    name,
                    15.0,
                    theme.text_primary,
                    transform * Affine::translate((card.x0 + 12.0, card.y0 + 124.0)),
                    Some((card.width() - 24.0) as f32),
                    TextWeight::Medium,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &format!("{count} photos · approved"),
                    12.0,
                    theme.text_dim,
                    transform * Affine::translate((card.x0 + 12.0, card.y0 + 146.0)),
                    Some((card.width() - 24.0) as f32),
                    TextWeight::Regular,
                );
            }
        }
        let suggestions: Vec<_> = self
            .people_controller
            .visible_clusters()
            .into_iter()
            .filter(|cluster| cluster.is_proposal())
            .take(6)
            .collect();
        let suggestion_y = if self.library_people.is_empty() {
            y + 44.0
        } else {
            let columns = if content.width() < 620.0 { 2 } else { 4 };
            y + (self.library_people.len().min(8).div_ceil(columns) as f64) * 184.0 + 12.0
        };
        text_renderer.draw_text_weighted(
            scene,
            "Local suggestions",
            17.0,
            theme.text_primary,
            transform * Affine::translate((content.x0, suggestion_y)),
            Some(content.width() as f32),
            TextWeight::Medium,
        );
        let suggestion_copy = if suggestions.is_empty() {
            "No local suggestions yet. Enable recognition, then scan a local batch.".to_owned()
        } else {
            format!(
                "{} suggestion{} waiting for a name and approval; none are shared.",
                suggestions.len(),
                if suggestions.len() == 1 { "" } else { "s" }
            )
        };
        text_renderer.draw_text_weighted(
            scene,
            &suggestion_copy,
            14.0,
            theme.text_dim,
            transform * Affine::translate((content.x0, suggestion_y + 25.0)),
            Some(content.width() as f32),
            TextWeight::Regular,
        );
        let progress = &self.people_scan_progress;
        let progress_copy = match progress.status.as_str() {
            "running" => format!(
                "Scanning locally: next batch starts at photo {} of {}. You can cancel; scanning resumes from there.",
                progress.next_offset.saturating_add(1),
                progress.last_library_size
            ),
            "cancelled" => format!(
                "Local scan paused after {} batch(es). Start Scan local library for faces to resume.",
                progress.completed_batches
            ),
            "error" => format!(
                "Last local scan failed: {}. Retry resumes at the next batch.",
                progress
                    .last_error
                    .as_deref()
                    .unwrap_or("local model unavailable")
            ),
            "paused" if progress.completed_batches > 0 => format!(
                "{} local batch(es) completed. Scan again to continue through this device's library.",
                progress.completed_batches
            ),
            _ => String::new(),
        };
        if !progress_copy.is_empty() {
            text_renderer.draw_text_weighted(
                scene,
                &progress_copy,
                12.0,
                theme.text_dim,
                transform * Affine::translate((content.x0, suggestion_y + 48.0)),
                Some(content.width() as f32),
                TextWeight::Regular,
            );
        }
    }

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
        self.library_page_hits.clear();
        self.timeline_density_hits.clear();
        self.library_details_cards.clear();
        self.places_hits.clear();
        if self.library_items.is_empty() {
            let (title, body) = if self.library_query.trim().is_empty() {
                (
                    tr("photos-library-empty-title"),
                    tr("photos-library-empty-body"),
                )
            } else {
                (
                    "No matching photographs".to_owned(),
                    "Try a different name, person, place, tag, or text phrase.".to_owned(),
                )
            };
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
            // Empty collections are useful before the first import too; keep
            // the same New album affordance available rather than stranding
            // the user behind an empty-state dead end.
            self.draw_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        let mut bounds = self.canvas_rect;
        // On a desktop the details sheet is a persistent side panel, not a
        // transient popover. Give it an actual layout lane so filters, cards,
        // and destructive controls never continue underneath an opaque sheet
        // and emerge as clipped half-targets at its edge.
        if self.metadata_editor && theme.text_scale <= 1.25 && bounds.width() >= 620.0 {
            bounds.x1 = (bounds.x1 - 432.0).max(bounds.x0);
        }
        // Enlarged text needs its own density rule. Five desktop cards leave
        // only a few words of metadata per card at 200%, which makes labels
        // overlap their independent destructive controls.
        let large_text = theme.text_scale > 1.25;
        let columns = timeline_column_count(bounds.width(), self.timeline_density, large_text);
        let gap = if columns <= 2 { 8.0 } else { 16.0 };
        let card_w =
            ((bounds.width() - f64::from(PAD * 2.0) - gap * columns.saturating_sub(1) as f64)
                / columns as f64)
                .max(1.0);
        let album_h = if self.library_albums.is_empty() {
            0.0
        } else if large_text {
            54.0
        } else {
            38.0
        };
        let desktop_library = !large_text && bounds.width() >= LIBRARY_TWO_COLUMN_MAX_W;
        let compact_header = compact_library_header_geometry(bounds, large_text, album_h > 0.0);
        let masthead = Rect::new(
            bounds.x0 + f64::from(PAD),
            bounds.y0 + 8.0,
            bounds.x1 - f64::from(PAD),
            bounds.y0 + 64.0,
        );
        if desktop_library {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &masthead,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &masthead,
            );
        }
        let heading_y = if desktop_library {
            masthead.y0 + 15.0
        } else {
            compact_header.title_y
        };
        text_renderer.draw_text_weighted(
            scene,
            &tr("photos-library"),
            if desktop_library { 24.0 } else { 18.0 },
            theme.text_primary,
            transform
                * Affine::translate((
                    if desktop_library {
                        masthead.x0 + 16.0
                    } else {
                        bounds.x0 + f64::from(PAD)
                    },
                    heading_y,
                )),
            Some((bounds.width() - f64::from(PAD * 2.0)) as f32),
            TextWeight::Medium,
        );
        // First-class destinations use the same stable local navigation
        // target rather than turning People/Places into a search suggestion.
        if desktop_library {
            let mut tab_x = masthead.x1 - 272.0;
            for (hit, label, selected) in [
                (
                    PlacesHit::Timeline,
                    "Timeline",
                    self.library_destination == LibraryDestination::Timeline,
                ),
                (
                    PlacesHit::People,
                    "People",
                    self.library_destination == LibraryDestination::People,
                ),
                (PlacesHit::Places, "Places", false),
            ] {
                let tab = Rect::new(tab_x, masthead.y0 + 13.0, tab_x + 78.0, masthead.y0 + 47.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if selected {
                        theme.tokens.colors.accent[6]
                    } else {
                        theme.tokens.colors.surfaces.window_card
                    },
                    None,
                    &tab,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.border_standard,
                    None,
                    &tab,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    11.0,
                    theme.text_primary,
                    transform * Affine::translate((tab.x0 + 9.0, tab.y0 + 10.0)),
                    Some(60.0),
                    TextWeight::Medium,
                );
                self.places_hits.push((hit, tab));
                tab_x += 84.0;
            }
        }
        let count = if desktop_library {
            format!("{} photographs · shared library", self.library_total)
        } else if large_text {
            format!("{} photos", self.library_total)
        } else if self.library_total == 0 {
            "0".to_owned()
        } else {
            let first = self.library_offset + 1;
            let last = (self.library_offset + self.library_items.len()).min(self.library_total);
            format!("{first}–{last} / {}", self.library_total)
        };
        let count_width = if desktop_library {
            260.0
        } else if large_text {
            180.0
        } else {
            100.0
        };
        text_renderer.draw_text_weighted(
            scene,
            &count,
            13.0,
            theme.text_dim,
            transform
                * Affine::translate((
                    if desktop_library {
                        masthead.x0 + 16.0
                    } else if large_text {
                        bounds.x0 + f64::from(PAD)
                    } else {
                        bounds.x1 - f64::from(PAD) - count_width
                    },
                    if desktop_library {
                        heading_y + 32.0
                    } else if large_text {
                        compact_header.count_y
                    } else {
                        heading_y + 3.0
                    },
                )),
            Some(count_width as f32),
            TextWeight::Regular,
        );
        // Density is a first-class browsing affordance, not a hidden gesture.
        // The two small controls are intentionally next to the count so they
        // remain discoverable on a touch display and at 200% text.
        let density_y = if desktop_library {
            masthead.y0 + 16.0
        } else {
            compact_header.controls.y0
        };
        let density_x = bounds.x1 - f64::from(PAD) - 96.0;
        for (hit, rect, enabled) in [
            (
                TimelineHit::ZoomOut,
                Rect::new(density_x, density_y, density_x + 44.0, density_y + 44.0),
                self.timeline_density > 0,
            ),
            (
                TimelineHit::ZoomIn,
                Rect::new(
                    density_x + 52.0,
                    density_y,
                    density_x + 96.0,
                    density_y + 44.0,
                ),
                self.timeline_density < 4,
            ),
        ] {
            if let Some((_, button)) = self
                .timeline_density_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, theme, presence);
            }
            if enabled {
                self.timeline_density_hits.push((hit, rect));
            }
        }
        if !self.library_albums.is_empty() {
            let mut x = bounds.x0 + f64::from(PAD) + if desktop_library { 60.0 } else { 0.0 };
            let y = if desktop_library {
                masthead.y1 + 13.0
            } else {
                compact_header.albums_y
            };
            if desktop_library {
                text_renderer.draw_text_weighted(
                    scene,
                    "ALBUMS",
                    10.0,
                    theme.text_dim,
                    transform * Affine::translate((bounds.x0 + f64::from(PAD), y + 7.0)),
                    Some(54.0),
                    TextWeight::Medium,
                );
            }
            let mut albums = vec![(
                None,
                if large_text {
                    "All".to_owned()
                } else {
                    format!("All photos ({})", self.library_total)
                },
            )];
            albums.extend(self.library_albums.iter().map(|(id, name, count)| {
                (
                    Some(id.clone()),
                    if large_text {
                        compact_label(name, 12)
                    } else {
                        format!("{name} ({count})")
                    },
                )
            }));
            for (album_id, label) in albums {
                let width = if large_text {
                    (label.chars().count() as f64 * 13.0 + 22.0).clamp(64.0, 190.0)
                } else {
                    (label.chars().count() as f64 * 7.2 + 18.0).min(150.0)
                };
                if x + width > bounds.x1 - f64::from(PAD) {
                    break;
                }
                let chip = Rect::new(x, y, x + width, y + if large_text { 44.0 } else { 26.0 });
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
                    transform
                        * Affine::translate((
                            chip.x0 + 8.0,
                            chip.y0 + if large_text { 10.0 } else { 6.0 },
                        )),
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
        let header_h = if desktop_library {
            56.0
        } else {
            (compact_header.timeline_y - heading_y - album_h).max(0.0)
        };
        let filter_y = heading_y + header_h + album_h;
        let mut filter_x = bounds.x0 + f64::from(PAD) + if desktop_library { 60.0 } else { 0.0 };
        // Paging remains for the album editor's bounded membership draft;
        // the normal timeline itself scrolls continuously by chronological row.
        let desktop_pager =
            self.album_editor && desktop_library && self.library_total > columns.saturating_mul(2);
        let filter_x1 = bounds.x1 - f64::from(PAD) - if desktop_pager { 192.0 } else { 0.0 };
        let compact_filters = columns <= 2;
        let mut filters = vec![(
            None,
            None,
            None,
            false,
            if compact_filters {
                "All".to_owned()
            } else {
                "All metadata".to_owned()
            },
        )];
        if large_text {
            // Search and collections are the compact navigation model at
            // 200%; do not add a third row of metadata chips.
        } else if compact_filters {
            // A phone has one shelf row. Show one folder *and* one explicit
            // people label rather than filling it with folders and hiding the
            // privacy-controlled people route.
            if let Some((folder, count)) = self.library_folders.first() {
                filters.push((
                    Some(folder.clone()),
                    None,
                    None,
                    false,
                    format!("{} ({count})", compact_label(folder, 8)),
                ));
            }
            if let Some((person, count)) = self.library_people.first() {
                filters.push((
                    None,
                    Some(person.clone()),
                    None,
                    false,
                    format!("{} ({count})", compact_label(person, 8)),
                ));
            }
        }
        if self.library_favorite_count > 0 {
            filters.insert(
                1,
                (
                    None,
                    None,
                    None,
                    true,
                    format!("Favorites ({})", self.library_favorite_count),
                ),
            );
        }
        for (folder, person, tag, favorites, label) in filters {
            // On desktop, collections live in the album rail and the date
            // rhythm does the browsing work.  This avoids a second dashboard
            // row of metadata chips above every photograph.
            if desktop_library || large_text {
                break;
            }
            let width = if large_text {
                (label.chars().count() as f64 * 13.0 + 22.0).clamp(64.0, 200.0)
            } else {
                (label.chars().count() as f64 * 6.6 + 18.0).clamp(
                    if compact_filters { 54.0 } else { 92.0 },
                    if compact_filters { 110.0 } else { 196.0 },
                )
            };
            if filter_x + width > filter_x1 {
                break;
            }
            let chip_h = if large_text { 44.0 } else { 28.0 };
            let chip = Rect::new(filter_x, filter_y, filter_x + width, filter_y + chip_h);
            let selected = folder.as_ref() == self.active_folder.as_ref()
                && person.as_ref() == self.active_person.as_ref()
                && tag.as_ref() == self.active_tag.as_ref()
                && self.active_favorites == favorites
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
            self.metadata_filter_cards.push(MetadataFilterCard {
                folder,
                person,
                tag,
                favorites,
                bounds: chip,
            });
            filter_x += width + 8.0;
        }
        if desktop_pager {
            let previous = Rect::new(
                filter_x1 + 8.0,
                filter_y - 8.0,
                filter_x1 + 92.0,
                filter_y + 36.0,
            );
            let next = Rect::new(
                previous.x1 + 8.0,
                filter_y - 8.0,
                previous.x1 + 92.0,
                filter_y + 36.0,
            );
            for (forward, rect, label) in [(false, previous, "Previous"), (true, next, "Next")] {
                let enabled = self.can_page_library(forward);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if enabled {
                        theme.tokens.colors.surfaces.raised
                    } else {
                        theme.tokens.colors.neutral[2]
                    },
                    None,
                    &rect,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.border_standard,
                    None,
                    &rect,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    11.0,
                    if enabled {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    },
                    transform * Affine::translate((rect.x0 + 9.0, rect.y0 + 13.0)),
                    Some((rect.width() - 18.0) as f32),
                    TextWeight::Medium,
                );
                self.library_page_hits.push((forward, enabled, rect));
            }
        }
        // Compact album actions now occupy the header's title lane (and the
        // enlarged-text variant has its own measured row). Do not preserve
        // the old beneath-the-chips reservation: it was an invisible 54px
        // gutter that made a one-day timeline look prematurely finished.
        let compact_album_actions_h = 0.0;
        let filter_h = if desktop_library || large_text {
            0.0
        } else {
            34.0
        };
        let pager_h = if large_text
            && bounds.width() < 620.0
            && self.album_editor
            && self.library_total > 1
        {
            52.0
        } else {
            0.0
        };
        let pager_y = heading_y + header_h + album_h + filter_h + compact_album_actions_h;
        if pager_h > 0.0 {
            let previous = Rect::new(
                bounds.x0 + f64::from(PAD),
                pager_y + 4.0,
                bounds.x0 + f64::from(PAD) + 116.0,
                pager_y + 46.0,
            );
            let next = Rect::new(
                bounds.x1 - f64::from(PAD) - 88.0,
                pager_y + 4.0,
                bounds.x1 - f64::from(PAD),
                pager_y + 46.0,
            );
            for (forward, rect, label) in [(false, previous, "Previous"), (true, next, "Next")] {
                let enabled = self.can_page_library(forward);
                scene.fill(
                    Fill::NonZero,
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
                    Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(rect.x0, rect.y1 - 1.0, rect.x1, rect.y1),
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
                    transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 10.0)),
                    Some((rect.width() - 20.0) as f32),
                    TextWeight::Medium,
                );
                self.library_page_hits.push((forward, enabled, rect));
            }
            text_renderer.draw_text_weighted(
                scene,
                &format!("{} / {}", self.library_offset + 1, self.library_total),
                12.0,
                theme.text_dim,
                transform * Affine::translate((previous.x1 + 10.0, pager_y + 14.0)),
                Some((next.x0 - previous.x1 - 20.0).max(1.0) as f32),
                TextWeight::Medium,
            );
        }
        // Selection owns an explicit action shelf above the cards. On a
        // narrow or enlarged-text window it becomes two comfortable rows
        // instead of compressing five actions into an unreadable strip.
        let selection_h = if self.selection_mode {
            if large_text || bounds.width() < 620.0 {
                if large_text { 144.0 } else { 100.0 }
            } else {
                52.0
            }
        } else {
            0.0
        };
        let selection_y = pager_y + pager_h;
        self.draw_selection_controls(
            scene,
            transform,
            text_renderer,
            theme,
            bounds,
            masthead,
            selection_y,
            selection_h,
            desktop_library,
            compact_header.controls,
            presence,
        );
        let top = selection_y + selection_h;
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
                // At 200% a wide two-column card would otherwise derive a
                // ~470 px preview from its width and leave no complete row
                // below the enlarged browse controls. Keep a generous,
                // aspect-preserving tray while ensuring the actions remain
                // reachable in an ordinary 900 px desktop window.
                if bounds.width() < 620.0 { 220.0 } else { 320.0 }
            } else if columns == 1 {
                320.0
            } else if desktop_library {
                // Two complete desktop rows are more useful than one large
                // row above a blank canvas. The card remains comfortably
                // wide at this three-column breakpoint.
                168.0
            } else {
                178.0
            },
        );
        // The footer has two text lines and an honest 44px action row. Keep
        // it roomy instead of letting desktop density turn controls into
        // loose glyphs or make metadata collide with a filename.
        let card_h = preview_h
            + if large_text {
                96.0
            } else if desktop_library {
                // The timeline is photography first.  Names sit in a small
                // caption band; the editing controls appear only on hover,
                // focus, or selection rather than consuming every tile.
                32.0
            } else {
                // Compact cards show a filename, one concise provenance
                // line, and three independent 44px actions. Give those
                // separate vertical lanes; the former 76px footer made the
                // metadata baseline collide with Details/Delete/Favorite.
                106.0
            };
        let rows = ((bounds.y1 - top - f64::from(PAD)).max(0.0) / (card_h + gap)).floor() as usize;
        // The index speaks in chronological *rows*, the exact continuous
        // coordinate used by the virtualised timeline. It is not derived from
        // `library_items.len()`: that is just the 120-item materialisation
        // window and would make the thumb lie about a large collection.
        let content_rows = self.library_total.div_ceil(columns.max(1));
        self.timeline_index_metrics = ScrollMetrics::new(
            f64::from(u32::try_from(content_rows).unwrap_or(u32::MAX)),
            f64::from(u32::try_from(rows.max(1)).unwrap_or(u32::MAX)),
            self.timeline_scroll_row,
        );
        self.timeline_index.set_rect(Rect::new(
            bounds.x1 - f64::from(PAD) - ui::widget::date_indexed_scrollbar::DEFAULT_TRACK_WIDTH,
            top + 4.0,
            bounds.x1 - f64::from(PAD),
            bounds.y1 - f64::from(PAD),
        ));
        // The editor is modal at large text: leaving a partially visible
        // ultra-wide thumbnail behind it creates a false second target and
        // can make its action row look clipped. The local grid decision still
        // drops to one lane, ready for the sheet to close, but the sheet owns
        // the visual surface while it is active.
        let visible = if large_text && self.metadata_editor {
            0
        } else {
            // Two overscan rows make a wheel/pinch density change feel
            // immediate while keeping rendering and thumbnail decoding bounded.
            timeline_virtual_range(self.library_items.len(), 0, rows, columns).len()
        };
        // The timeline itself may be fractionally translated by a wheel or
        // trackpad. Clip that moving content to the measured canvas region so
        // a fast scroll can never paint over the search/header or status bar.
        let timeline_clip = Rect::new(bounds.x0, top, bounds.x1, bounds.y1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &timeline_clip,
        );
        let mut timeline_y =
            top - (self.timeline_scroll_row - self.timeline_scroll_row.floor()) * (card_h + gap);
        let mut timeline_column = columns;
        let mut previous_day = String::new();
        let mut painted_items = 0usize;
        for item in self.library_items.iter().take(visible) {
            let id = &item.id;
            let name = &item.name;
            let favorite = item.favorite;
            if item.captured_at != previous_day {
                let group_top = timeline_y
                    + if timeline_column == columns {
                        0.0
                    } else {
                        card_h + gap
                    };
                // Do not leave an orphan date label at the bottom of a
                // viewport; every heading earns room for its first photo.
                if group_top + 34.0 + card_h > bounds.y1 - f64::from(PAD) {
                    break;
                }
                timeline_y = group_top;
                let day_label = timeline_date_label(&item.captured_at);
                let index_hint = if self.timeline_index_metrics.overflows() {
                    "Jump by date →"
                } else {
                    "One-day view"
                };
                let hint_width = if large_text { 138.0 } else { 112.0 };
                let hint_x = bounds.x1 - f64::from(PAD) - hint_width - 18.0;
                let day_rule = Rect::new(
                    bounds.x0 + f64::from(PAD),
                    timeline_y + 24.0,
                    hint_x - 8.0,
                    timeline_y + 25.0,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &day_label,
                    if large_text { 15.0 } else { 13.0 },
                    theme.text_primary,
                    transform * Affine::translate((day_rule.x0, timeline_y + 4.0)),
                    Some((day_rule.width() - 84.0) as f32),
                    TextWeight::Medium,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(day_rule.x0 + 148.0, day_rule.y0, day_rule.x1, day_rule.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    index_hint,
                    if large_text { 13.0 } else { 11.0 },
                    theme.text_dim,
                    transform * Affine::translate((hint_x, timeline_y + 5.0)),
                    Some(hint_width as f32),
                    TextWeight::Medium,
                );
                timeline_y += 34.0;
                timeline_column = 0;
                previous_day.clone_from(&item.captured_at);
            }
            if timeline_y + card_h > bounds.y1 - f64::from(PAD) {
                break;
            }
            let x = bounds.x0 + f64::from(PAD) + timeline_column as f64 * (card_w + gap);
            let y = timeline_y;
            let card = Rect::new(x, y, x + card_w, y + card_h);
            let preview = Rect::new(x, y, x + card_w, y + preview_h);
            scene.fill(
                Fill::NonZero,
                transform,
                if card.contains(self.pointer) {
                    theme.tokens.colors.neutral[3]
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &card,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[2],
                None,
                &preview,
            );
            if let Some(image) = self.library_thumbnails.get(id) {
                let scale = (preview.width() / f64::from(image.width))
                    .min(preview.height() / f64::from(image.height));
                let drawn_w = f64::from(image.width) * scale;
                let drawn_h = f64::from(image.height) * scale;
                let image_transform = transform
                    * Affine::translate((
                        preview.x0 + (preview.width() - drawn_w) * 0.5,
                        preview.y0 + (preview.height() - drawn_h) * 0.5,
                    ))
                    * Affine::scale(scale);
                scene.draw_image(image, image_transform);
            } else {
                // Decoding happens off the UI thread and is deliberately
                // bounded to one small job per sync.  A quiet image glyph is
                // an honest loading/unavailable placeholder that keeps a
                // shelf recognisable while it catches up, instead of a row
                // of anonymous empty rectangles.
                let icon_side = preview.width().min(preview.height()).min(34.0);
                let icon_rect = Rect::from_center_size(preview.center(), (icon_side, icon_side));
                draw_icon(
                    scene,
                    transform,
                    icon_rect,
                    Icon::Image,
                    theme.text_dim,
                    1.5,
                );
            }
            let footer = Rect::new(card.x0, preview.y1, card.x1, card.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &footer,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(footer.x0, footer.y0, footer.x1, footer.y0 + 1.0),
            );
            let action_y = card.y1 - LIBRARY_ACTION_H;
            let control_w = LIBRARY_ACTION_H;
            let details = Rect::new(
                card.x0 + 8.0,
                action_y,
                (card.x0 + if desktop_library { 136.0 } else { 108.0 })
                    .min(card.x1 - control_w * 2.0),
                card.y1,
            );
            let remove = Rect::new(
                card.x1 - control_w * 2.0,
                action_y,
                card.x1 - control_w,
                card.y1,
            );
            let star = Rect::new(card.x1 - control_w, action_y, card.x1, card.y1);
            // Both text rows are explicitly constrained to this card. The
            // action row owns the bottom 44px, so no title can collide with a
            // neighboring column or a control.
            let display_name = compact_label(
                name,
                ((card_w - 18.0) / if large_text { 16.0 } else { 8.0 })
                    .floor()
                    .max(8.0) as usize,
            );
            text_renderer.draw_text_weighted(
                scene,
                &display_name,
                if desktop_library { 14.0 } else { 13.0 },
                theme.text_primary,
                transform
                    * Affine::translate((x + 9.0, preview.y1 + if large_text { 8.0 } else { 9.0 })),
                Some((card_w - 18.0) as f32),
                TextWeight::Medium,
            );
            if !desktop_library && !large_text && !item.metadata.is_empty() {
                let metadata = compact_label(
                    &item.metadata,
                    ((card_w - 18.0) / if large_text { 14.0 } else { 6.5 })
                        .floor()
                        .max(12.0) as usize,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &metadata,
                    if desktop_library { 12.0 } else { 11.0 },
                    theme.text_dim,
                    transform
                        * Affine::translate((
                            x + 9.0,
                            preview.y1 + if large_text { 34.0 } else { 29.0 },
                        )),
                    Some((card_w - 18.0) as f32),
                    TextWeight::Regular,
                );
            }
            if !self.album_editor && !self.selection_mode {
                if !desktop_library || card.contains(self.pointer) {
                    // Desktop controls are deliberately transient: the
                    // photograph remains the dominant surface until someone
                    // intends to act on it.
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
                    text_renderer.draw_text_weighted(
                        scene,
                        "Details",
                        12.0,
                        theme.tokens.colors.accent[6],
                        transform * Affine::translate((details.x0 + 28.0, details.y0 + 15.0)),
                        Some((details.width() - 30.0) as f32),
                        TextWeight::Medium,
                    );
                    draw_icon(
                        scene,
                        transform,
                        Rect::new(
                            details.x0 + 10.0,
                            details.y0 + 16.0,
                            details.x0 + 26.0,
                            details.y0 + 32.0,
                        ),
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
                }
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
            if self.selection_mode && self.selected_photo_ids.contains(id) {
                scene.stroke(
                    &Stroke::new(3.0),
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &card,
                );
                let badge = Point::new(card.x1 - 24.0, card.y0 + 24.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Circle::new(badge, 15.0),
                );
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(badge, (18.0, 18.0)),
                    Icon::Check,
                    theme.tokens.colors.surfaces.base,
                    2.0,
                );
            }
            self.library_cards.push((id.clone(), card, favorite));
            painted_items += 1;
            timeline_column += 1;
            if timeline_column == columns {
                timeline_y += card_h + gap;
                timeline_column = 0;
            }
        }
        // Never leave a short final viewport looking like a failed virtual
        // list. The continuation/end card is derived from the actual next
        // record and bounded materialisation window — no inferred memories,
        // locations, or dates are fabricated here.
        let remaining = self.library_items.len().saturating_sub(painted_items);
        let can_summarise = remaining > 0
            || (self.timeline_index_markers.len() == 1
                && self.library_items.len() == self.library_total
                && self.library_items.len() < visible
                && timeline_column == 0);
        if can_summarise {
            let summary_y = timeline_y
                + if timeline_column == 0 {
                    2.0
                } else {
                    card_h + gap + 2.0
                };
            let summary_bottom = (summary_y + 122.0).min(bounds.y1 - f64::from(PAD));
            if summary_bottom - summary_y >= 82.0 {
                let summary = Rect::new(
                    bounds.x0 + f64::from(PAD),
                    summary_y,
                    bounds.x1 - f64::from(PAD),
                    summary_bottom,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &summary,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.border_standard,
                    None,
                    &summary,
                );
                let (title, body) = if let Some(next) = self.library_items.get(painted_items) {
                    let remaining_total = self.library_total.saturating_sub(painted_items);
                    let count = if remaining_total == 1 {
                        "1 photo remains".to_owned()
                    } else {
                        format!("{remaining_total} photos remain")
                    };
                    (
                        format!("Continue to {}", timeline_date_label(&next.captured_at)),
                        format!("{count} · Scroll, drag the date index, or choose a date marker."),
                    )
                } else if self.library_total == 1 {
                    (
                        "This day is complete · 1 photo shown".to_owned(),
                        "Use + / −, pinch, or Ctrl + wheel to change timeline density.".to_owned(),
                    )
                } else {
                    (
                        format!("This day is complete · {} photos shown", self.library_total),
                        "Use + / −, pinch, or Ctrl + wheel to change timeline density.".to_owned(),
                    )
                };
                text_renderer.draw_text_weighted(
                    scene,
                    &title,
                    if large_text { 15.0 } else { 13.0 },
                    theme.text_primary,
                    transform * Affine::translate((summary.x0 + 14.0, summary.y0 + 15.0)),
                    Some((summary.width() - 28.0) as f32),
                    TextWeight::Medium,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &body,
                    if large_text { 13.0 } else { 11.0 },
                    theme.text_dim,
                    transform * Affine::translate((summary.x0 + 14.0, summary.y0 + 43.0)),
                    Some((summary.width() - 28.0) as f32),
                    TextWeight::Regular,
                );
            }
        }
        scene.pop_layer();
        if !self.album_editor && !self.metadata_editor && !self.selection_album_chooser {
            self.timeline_index.paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.timeline_index_metrics,
                &self.timeline_index_markers,
            );
        }
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
        self.draw_metadata_editor(scene, transform, text_renderer, theme, presence);
        self.draw_selection_destination_sheet(scene, transform, text_renderer, theme, bounds);
    }

    /// Render the location library as an offline-first map or an equivalent
    /// list. The map is a reference-grid substrate, not a disguised online
    /// tile service: opening Places never sends coordinates off-device.
    fn draw_places(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;
        self.places_hits.clear();
        let bounds = Rect::new(
            self.canvas_rect.x0 + 10.0,
            self.canvas_rect.y0 + 10.0,
            self.canvas_rect.x1 - 10.0,
            self.canvas_rect.y1 - 10.0,
        );
        let heading = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y0 + 58.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &heading,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &heading,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Places",
            22.0,
            theme.text_primary,
            transform * Affine::translate((heading.x0 + 14.0, heading.y0 + 12.0)),
            Some(180.0),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!(
                "{} photos with an explicitly shared location",
                self.places_items.len()
            ),
            12.0,
            theme.text_dim,
            transform * Affine::translate((heading.x0 + 14.0, heading.y0 + 36.0)),
            Some((heading.width() - 310.0).max(80.0) as f32),
            TextWeight::Regular,
        );
        let mut x = heading.x1 - 300.0;
        for (hit, label) in [
            (PlacesHit::Timeline, "Timeline"),
            (PlacesHit::People, "People"),
            (PlacesHit::Places, "Places"),
        ] {
            let active = matches!(hit, PlacesHit::Places);
            let rect = Rect::new(x, heading.y0 + 10.0, x + 78.0, heading.y0 + 46.0);
            scene.fill(
                Fill::NonZero,
                transform,
                if active {
                    theme.tokens.colors.accent[6]
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                11.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 9.0, rect.y0 + 11.0)),
                Some(60.0),
                TextWeight::Medium,
            );
            self.places_hits.push((hit, rect));
            x += 84.0;
        }
        if self.places_items.is_empty() {
            self.draw_empty_state(scene, transform, text_renderer, theme, "No places yet", "Import photos with GPS metadata or add an explicit shared location in photo details. Photos does not guess or upload locations.");
            return;
        }
        let map = Rect::new(bounds.x0, heading.y1 + 10.0, bounds.x1, bounds.y1);
        self.places_map_bounds = map;
        let toggle = Rect::new(map.x1 - 112.0, map.y0 + 12.0, map.x1 - 12.0, map.y0 + 48.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &toggle,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &toggle,
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.places_list_mode { "Map" } else { "List" },
            12.0,
            theme.text_primary,
            transform * Affine::translate((toggle.x0 + 28.0, toggle.y0 + 11.0)),
            Some(60.0),
            TextWeight::Medium,
        );
        self.places_hits.push((PlacesHit::ToggleList, toggle));
        if self.places_list_mode {
            self.draw_places_list(scene, transform, text_renderer, theme, map);
            return;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.neutral[2],
            None,
            &map,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &map,
        );
        // Calm offline geographic grid: it retains spatial orientation while
        // clearly avoiding a claim that live roads/tiles were downloaded.
        for step in 1..10 {
            let fx = map.x0 + map.width() * f64::from(step) / 10.0;
            let fy = map.y0 + map.height() * f64::from(step) / 10.0;
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(fx, map.y0, fx + 1.0, map.y1),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(map.x0, fy, map.x1, fy + 1.0),
            );
        }
        text_renderer.draw_text_weighted(
            scene,
            "Offline map · no map provider is contacted",
            11.0,
            theme.text_dim,
            transform * Affine::translate((map.x0 + 12.0, map.y1 - 22.0)),
            Some(260.0),
            TextWeight::Regular,
        );
        let zoom_out = Rect::new(map.x0 + 12.0, map.y0 + 12.0, map.x0 + 52.0, map.y0 + 52.0);
        let zoom_in = Rect::new(map.x0 + 12.0, map.y0 + 58.0, map.x0 + 52.0, map.y0 + 98.0);
        for (hit, rect, label) in [
            (PlacesHit::ZoomOut, zoom_out, "−"),
            (PlacesHit::ZoomIn, zoom_in, "+"),
        ] {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                20.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 14.0, rect.y0 + 6.0)),
                Some(16.0),
                TextWeight::Medium,
            );
            self.places_hits.push((hit, rect));
        }
        let photos: Vec<_> = self
            .places_items
            .iter()
            .map(|item| item.photo.clone())
            .collect();
        for cluster in crate::places::clusters(&photos, self.places_viewport.zoom) {
            let location = crate::model::PhotoLocation {
                latitude: cluster.latitude,
                longitude: cluster.longitude,
                label: Some(cluster.label.clone()),
                provenance: crate::model::LocationProvenance::default(),
            };
            let Some((px, py)) = self
                .places_viewport
                .project(&location, map.width(), map.height())
            else {
                continue;
            };
            let point = Point::new(map.x0 + px, map.y0 + py);
            if !map.contains(point) {
                continue;
            }
            let radius = (14.0 + (cluster.count() as f64).sqrt() * 3.0).min(28.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[6],
                None,
                &Circle::new(point, radius),
            );
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &Circle::new(point, radius),
            );
            if let Some(photo_id) = cluster.photo_ids.first()
                && let Some(image) = self.library_thumbnails.get(photo_id)
            {
                let side = (radius * 1.25).max(18.0);
                let scale = (side / f64::from(image.width)).min(side / f64::from(image.height));
                let image_transform = transform
                    * Affine::translate((
                        point.x - f64::from(image.width) * scale * 0.5,
                        point.y - f64::from(image.height) * scale * 0.5,
                    ))
                    * Affine::scale(scale);
                scene.draw_image(image, image_transform);
            }
            text_renderer.draw_text_weighted(
                scene,
                &cluster.count().to_string(),
                12.0,
                theme.text_primary,
                transform * Affine::translate((point.x - 4.0, point.y - 7.0)),
                Some(16.0),
                TextWeight::Medium,
            );
            let hit = Rect::new(
                point.x - radius,
                point.y - radius,
                point.x + radius,
                point.y + radius,
            );
            if let Some(photo_id) = cluster.photo_ids.first() {
                self.places_hits
                    .push((PlacesHit::Marker(photo_id.clone()), hit));
            }
        }
    }

    fn draw_places_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        map: Rect,
    ) {
        use vello::peniko::Fill;
        let mut y = map.y0 + 64.0;
        for item in self.places_items.iter().take(8) {
            if y + 58.0 > map.y1 {
                break;
            }
            let row = Rect::new(map.x0 + 12.0, y, map.x1 - 12.0, y + 50.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &row,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &row,
            );
            text_renderer.draw_text_weighted(
                scene,
                &item.photo.image.name,
                14.0,
                theme.text_primary,
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 8.0)),
                Some((row.width() - 220.0) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                &format!(
                    "{} · {}",
                    item.photo.location.as_ref().map_or_else(
                        || "Shared location".to_owned(),
                        crate::model::PhotoLocation::display_name
                    ),
                    item.photo.captured_at.as_deref().unwrap_or("Undated")
                ),
                11.0,
                theme.text_dim,
                transform * Affine::translate((row.x0 + 12.0, row.y0 + 29.0)),
                Some((row.width() - 220.0) as f32),
                TextWeight::Regular,
            );
            self.places_hits
                .push((PlacesHit::Marker(item.photo.id.clone()), row));
            y += 58.0;
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the selection shelf keeps its responsive geometry and semantic hit boxes in one audited renderer"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "the responsive selection task bar keeps its painted controls and exact pointer hit geometry in one audited component"
    )]
    fn draw_selection_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
        masthead: Rect,
        selection_y: f64,
        selection_h: f64,
        desktop_library: bool,
        compact_selection_rect: Rect,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        self.selection_hits.clear();
        self.selection_destination_hits.clear();
        let draw_button = |scene: &mut Scene,
                           text_renderer: &mut TextRenderer,
                           hit: SelectionHit,
                           rect: Rect,
                           label: &str,
                           enabled: bool,
                           hits: &mut Vec<(SelectionHit, Rect)>| {
            scene.fill(
                Fill::NonZero,
                transform,
                if enabled {
                    theme.tokens.colors.surfaces.raised
                } else {
                    theme.tokens.colors.neutral[2]
                },
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
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
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 13.0)),
                Some((rect.width() - 20.0) as f32),
                TextWeight::Medium,
            );
            if enabled {
                hits.push((hit, rect));
            }
        };

        if !self.selection_mode {
            // The normal desktop masthead has intentional empty room between
            // its library identity and album management. On compact layouts
            // this short, named control follows the heading rather than
            // replacing a destructive card affordance.
            let rect = if desktop_library {
                Rect::new(
                    masthead.x1 - 500.0,
                    masthead.y0 + 16.0,
                    masthead.x1 - 388.0,
                    masthead.y1 - 16.0,
                )
            } else {
                compact_selection_rect
            };
            self.library_select_button.apply_layout_external(rect);
            self.library_select_button
                .draw(scene, transform, text_renderer, theme, presence);
            self.selection_hits.push((SelectionHit::Start, rect));
            return;
        }

        let selected = self.selected_photo_ids.len();
        let destination_label = self
            .selection_destination_album
            .as_deref()
            .and_then(|id| {
                self.library_albums
                    .iter()
                    .find(|(album_id, _, _)| album_id == id)
                    .map(|(_, name, _)| name)
            })
            .map_or_else(
                || "Choose album".to_owned(),
                |name| format!("Add to {}", compact_label(name, 12)),
            );
        let wide = selection_h <= 52.0;
        let x = bounds.x0 + f64::from(PAD);
        let max_x = bounds.x1 - f64::from(PAD);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &Rect::new(x, selection_y, max_x, selection_y + selection_h - 4.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &Rect::new(x, selection_y, max_x, selection_y + selection_h - 4.0),
        );
        let label = format!("{selected} selected");
        if wide {
            text_renderer.draw_text_weighted(
                scene,
                &label,
                13.0,
                theme.text_primary,
                transform * Affine::translate((x + 12.0, selection_y + 15.0)),
                Some(112.0),
                TextWeight::Medium,
            );
            let mut button_x = x + 122.0;
            for (hit, label, width, enabled) in [
                (
                    SelectionHit::SelectAll,
                    "Select all",
                    102.0,
                    !self.library_cards.is_empty(),
                ),
                (SelectionHit::Clear, "Clear", 76.0, selected > 0),
                (
                    SelectionHit::Favorite,
                    if self.selection_all_favorite {
                        "Unfavorite"
                    } else {
                        "Favorite"
                    },
                    104.0,
                    selected > 0,
                ),
                (
                    if self.selection_destination_album.is_some() {
                        SelectionHit::AddToAlbum
                    } else {
                        SelectionHit::ChooseAlbum
                    },
                    destination_label.as_str(),
                    128.0,
                    selected > 0 && !self.library_albums.is_empty(),
                ),
                (SelectionHit::Done, "Done", 78.0, true),
            ] {
                let rect = Rect::new(
                    button_x,
                    selection_y + 4.0,
                    button_x + width,
                    selection_y + 44.0,
                );
                draw_button(
                    scene,
                    text_renderer,
                    hit,
                    rect,
                    label,
                    enabled,
                    &mut self.selection_hits,
                );
                button_x += width + 6.0;
            }
        } else {
            text_renderer.draw_text_weighted(
                scene,
                &label,
                12.0,
                theme.text_primary,
                transform * Affine::translate((x + 10.0, selection_y + 9.0)),
                Some(96.0),
                TextWeight::Medium,
            );
            let first_y = selection_y + 4.0;
            let second_y = selection_y + 50.0;
            for (hit, label, rect, enabled) in [
                (
                    SelectionHit::SelectAll,
                    "All",
                    Rect::new(x + 104.0, first_y, x + 168.0, first_y + 40.0),
                    !self.library_cards.is_empty(),
                ),
                (
                    SelectionHit::Clear,
                    "Clear",
                    Rect::new(x + 174.0, first_y, x + 246.0, first_y + 40.0),
                    selected > 0,
                ),
                (
                    SelectionHit::Done,
                    "Done",
                    Rect::new(max_x - 78.0, first_y, max_x, first_y + 40.0),
                    true,
                ),
                (
                    SelectionHit::Favorite,
                    if self.selection_all_favorite {
                        "Unfavorite"
                    } else {
                        "Favorite"
                    },
                    Rect::new(x, second_y, x + 108.0, second_y + 40.0),
                    selected > 0,
                ),
                (
                    if self.selection_destination_album.is_some() {
                        SelectionHit::AddToAlbum
                    } else {
                        SelectionHit::ChooseAlbum
                    },
                    destination_label.as_str(),
                    Rect::new(x + 114.0, second_y, x + 248.0, second_y + 40.0),
                    selected > 0 && !self.library_albums.is_empty(),
                ),
            ] {
                draw_button(
                    scene,
                    text_renderer,
                    hit,
                    rect,
                    label,
                    enabled,
                    &mut self.selection_hits,
                );
            }
        }
    }

    /// The album picker is a real modal surface, not a third row packed into
    /// the selection bar. That keeps 200% text, paging, and the card grid
    /// from competing for the same few pixels. The underlying selection stays
    /// intact but is deliberately inert until this sheet closes.
    #[allow(
        clippy::too_many_lines,
        reason = "the modal keeps responsive layout, drawing, paging, and exact hit geometry together so they cannot drift"
    )]
    fn draw_selection_destination_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bounds: Rect,
    ) {
        use vello::peniko::Fill;

        self.selection_destination_hits.clear();
        if !self.library_mode || !self.selection_album_chooser {
            return;
        }
        // The sheet owns the interaction tree while it is visible. The
        // selection bar remains visible behind the scrim for context, but it
        // cannot receive a second, conflicting command.
        self.selection_hits.clear();
        let page_count = self
            .library_albums
            .len()
            .div_ceil(SELECTION_ALBUMS_PER_PAGE);
        self.selection_album_page = self.selection_album_page.min(page_count.saturating_sub(1));
        let start = self.selection_album_page * SELECTION_ALBUMS_PER_PAGE;
        let end = (start + SELECTION_ALBUMS_PER_PAGE).min(self.library_albums.len());
        let narrow = theme.text_scale > 1.25 || bounds.width() < 700.0;
        let sheet = if narrow {
            Rect::new(
                bounds.x0 + f64::from(PAD),
                bounds.y0 + 54.0,
                bounds.x1 - f64::from(PAD),
                bounds.y1 - 54.0,
            )
        } else {
            Rect::from_center_size(bounds.center(), (600.0, 540.0))
        };
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.neutral[0].with_alpha(0.78),
            None,
            &bounds,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(sheet, 14.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &RoundedRect::from_rect(sheet, 14.0),
        );
        let content_left = sheet.x0 + if narrow { 20.0 } else { 18.0 };
        text_renderer.draw_text_weighted(
            scene,
            "Choose destination album",
            17.0,
            theme.text_primary,
            transform * Affine::translate((content_left, sheet.y0 + 20.0)),
            Some((sheet.x1 - content_left - 18.0) as f32),
            TextWeight::Medium,
        );
        let summary = format!(
            "Albums {}–{} of {}",
            start + 1,
            end,
            self.library_albums.len()
        );
        text_renderer.draw_text_weighted(
            scene,
            &summary,
            12.0,
            theme.text_dim,
            transform
                * Affine::translate((content_left, sheet.y0 + if narrow { 61.0 } else { 49.0 })),
            Some((sheet.x1 - content_left - 18.0) as f32),
            TextWeight::Regular,
        );
        let footer_y = sheet.y1 - if narrow { 118.0 } else { 58.0 };
        let list_top = sheet.y0 + if narrow { 102.0 } else { 76.0 };
        let list_bottom = footer_y - 12.0;
        let row_h = ((list_bottom - list_top) / SELECTION_ALBUMS_PER_PAGE as f64).clamp(
            if narrow { 58.0 } else { 42.0 },
            if narrow { 66.0 } else { 54.0 },
        );
        let visible_albums = self.library_albums.get(start..end).unwrap_or_default();
        for (index, (id, name, count)) in visible_albums.iter().enumerate() {
            let y = list_top + index as f64 * row_h;
            let row = Rect::new(sheet.x0 + 12.0, y, sheet.x1 - 12.0, y + row_h - 6.0);
            let chosen = self.selection_destination_album.as_deref() == Some(id);
            scene.fill(
                Fill::NonZero,
                transform,
                if chosen {
                    theme.tokens.colors.accent[6]
                } else {
                    theme.tokens.colors.neutral[3]
                },
                None,
                &RoundedRect::from_rect(row, 9.0),
            );
            scene.stroke(
                &Stroke::new(if chosen { 2.0 } else { 1.0 }),
                transform,
                if chosen {
                    theme.tokens.colors.accent[8]
                } else {
                    theme.border_standard
                },
                None,
                &RoundedRect::from_rect(row, 9.0),
            );
            text_renderer.draw_text_weighted(
                scene,
                name,
                13.0,
                theme.text_primary,
                transform * Affine::translate((row.x0 + 14.0, row.y0 + 12.0)),
                Some((row.width() - if narrow { 166.0 } else { 126.0 }) as f32),
                TextWeight::Medium,
            );
            text_renderer.draw_text_weighted(
                scene,
                &format!("{count} photos"),
                11.0,
                if chosen {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                transform
                    * Affine::translate((
                        row.x1 - if narrow { 142.0 } else { 98.0 },
                        row.y0 + 14.0,
                    )),
                Some(if narrow { 126.0 } else { 84.0 }),
                TextWeight::Regular,
            );
            self.selection_destination_hits.push((id.clone(), row));
        }
        let button = |scene: &mut Scene,
                      text_renderer: &mut TextRenderer,
                      rect: Rect,
                      label: &str,
                      enabled: bool| {
            let shape = RoundedRect::from_rect(rect, 9.0);
            scene.fill(
                Fill::NonZero,
                transform,
                if enabled {
                    theme.tokens.colors.surfaces.window_card
                } else {
                    theme.tokens.colors.neutral[2]
                },
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &shape,
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
                transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 12.0)),
                Some((rect.width() - 22.0) as f32),
                TextWeight::Medium,
            );
        };
        let (previous, next, cancel, apply) = if narrow {
            let gap = 8.0;
            let half = (sheet.width() - 24.0 - gap) / 2.0;
            let left = sheet.x0 + 12.0;
            let right = sheet.x1 - 12.0;
            (
                Rect::new(left, footer_y, left + half, footer_y + 46.0),
                Rect::new(left + half + gap, footer_y, right, footer_y + 46.0),
                Rect::new(left, footer_y + 54.0, left + half, footer_y + 100.0),
                Rect::new(left + half + gap, footer_y + 54.0, right, footer_y + 100.0),
            )
        } else {
            (
                Rect::new(sheet.x0 + 12.0, footer_y, sheet.x0 + 112.0, footer_y + 42.0),
                Rect::new(
                    sheet.x0 + 120.0,
                    footer_y,
                    sheet.x0 + 202.0,
                    footer_y + 42.0,
                ),
                Rect::new(
                    sheet.x1 - 222.0,
                    footer_y,
                    sheet.x1 - 126.0,
                    footer_y + 42.0,
                ),
                Rect::new(sheet.x1 - 118.0, footer_y, sheet.x1 - 12.0, footer_y + 42.0),
            )
        };
        button(
            scene,
            text_renderer,
            previous,
            "Previous",
            self.selection_album_page > 0,
        );
        button(
            scene,
            text_renderer,
            next,
            "Next",
            end < self.library_albums.len(),
        );
        button(scene, text_renderer, cancel, "Cancel", true);
        button(
            scene,
            text_renderer,
            apply,
            "Use album",
            self.selection_destination_album.is_some(),
        );
        if self.selection_album_page > 0 {
            self.selection_hits
                .push((SelectionHit::PreviousDestinationPage, previous));
        }
        if end < self.library_albums.len() {
            self.selection_hits
                .push((SelectionHit::NextDestinationPage, next));
        }
        self.selection_hits
            .push((SelectionHit::CancelDestinationChooser, cancel));
        if self.selection_destination_album.is_some() {
            self.selection_hits
                .push((SelectionHit::ApplyDestination, apply));
        }
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
        // At enlarged text, keep album management visible rather than making
        // a core workflow search/agent-only. Its dedicated row is part of
        // `CompactHeaderGeometry`, so it cannot collide with the collection
        // rail above or the date heading below.
        if !self.album_editor && theme.text_scale > 1.25 {
            let bounds = self.canvas_rect;
            let row =
                compact_library_header_geometry(bounds, true, !self.library_albums.is_empty())
                    .album_actions;
            let button_w = (row.width() - 8.0) * 0.5;
            let new = Rect::new(row.x0, row.y0, row.x0 + button_w, row.y1);
            let edit = Rect::new(new.x1 + 8.0, row.y0, row.x1, row.y1);
            for (hit, rect, enabled) in [
                (AlbumEditorHit::New, new, true),
                (AlbumEditorHit::Edit, edit, self.active_album.is_some()),
            ] {
                if let Some((_, button)) = self
                    .library_album_buttons
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == hit)
                {
                    button.set_enabled(enabled);
                    button.apply_layout_external(rect);
                    button.draw(scene, transform, text_renderer, theme, presence);
                }
                if enabled {
                    self.album_editor_hits.push((hit, rect));
                }
            }
            self.album_editor_rect = Rect::new(new.x0, new.y0, edit.x1, edit.y1);
            return;
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 360.0 };
        // A phone must keep actual photographs reachable while editing; a
        // compact bottom sheet leaves the upper grid visible instead of
        // turning “choose photographs” into an instruction with no surface.
        let panel_h = if !self.album_editor && narrow {
            // Album management is an ordinary library route, not a tiny
            // desktop-only affordance. Keep its buttons at the same 44px
            // target as the card actions below it.
            44.0
        } else if narrow {
            // The editor has two paging controls and a footer. Their 44px
            // targets need a real gap rather than being squeezed into the
            // former 34px desktop rhythm.
            280.0
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
        let desktop_header_actions = !self.album_editor
            && !narrow
            && theme.text_scale <= 1.25
            && bounds.width() >= LIBRARY_TWO_COLUMN_MAX_W;
        let panel = if desktop_header_actions {
            // The normal wide masthead already reserves its right side for
            // destination, selection and density controls. Album management
            // belongs beside the title, where it remains discoverable without
            // painting over those independent actions.
            Rect::new(
                bounds.x0 + 132.0,
                bounds.y0 + 10.0,
                bounds.x0 + 366.0,
                bounds.y0 + 54.0,
            )
        } else {
            Rect::new(
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
            )
        };
        self.album_editor_rect = panel;
        if !self.album_editor {
            let new = Rect::new(panel.x0, panel.y0, panel.x0 + 110.0, panel.y0 + 44.0);
            let edit = Rect::new(new.x1 + 8.0, panel.y0, new.x1 + 116.0, panel.y0 + 44.0);
            for (hit, rect, enabled) in [
                (AlbumEditorHit::New, new, true),
                (AlbumEditorHit::Edit, edit, self.active_album.is_some()),
            ] {
                if let Some((_, button)) = self
                    .library_album_buttons
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == hit)
                {
                    button.set_enabled(enabled);
                    button.apply_layout_external(rect);
                    button.draw(scene, transform, text_renderer, theme, presence);
                }
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
        text_renderer.draw_text_weighted(
            scene,
            &format!("{} selected", self.album_editor_members.len()),
            13.0,
            theme.text_dim,
            transform * Affine::translate((panel.x1 - 116.0, panel.y0 + 147.0)),
            Some(100.0),
            TextWeight::Regular,
        );
        let page_y = panel.y0 + 166.0;
        let previous = Rect::new(panel.x0 + 16.0, page_y, panel.x0 + 108.0, page_y + 44.0);
        let next = Rect::new(panel.x1 - 108.0, page_y, panel.x1 - 16.0, page_y + 44.0);
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
        let y = panel.y1 - 60.0;
        let cancel = Rect::new(panel.x0 + 16.0, y, panel.x0 + 88.0, y + 44.0);
        let save = Rect::new(panel.x1 - 92.0, y, panel.x1 - 16.0, y + 44.0);
        let delete = Rect::new(save.x0 - 88.0, y, save.x0 - 8.0, y + 44.0);
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
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 14.0)),
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
                transform * Affine::translate((delete.x0 + 7.0, delete.y0 + 14.0)),
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
            "Choose photos",
            11.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 190.0)),
            Some((panel.width() - 180.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!("{} selected", self.album_editor_members.len()),
            12.0,
            theme.text_primary,
            transform * Affine::translate((panel.x1 - 146.0, panel.y0 + 190.0)),
            Some(130.0),
            TextWeight::Medium,
        );
        let page_y = panel.y0 + 222.0;
        let previous = Rect::new(panel.x0 + 16.0, page_y, panel.x0 + 132.0, page_y + 48.0);
        let next = Rect::new(panel.x0 + 140.0, page_y, panel.x0 + 256.0, page_y + 48.0);
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
                transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 12.0)),
                Some((rect.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            if enabled {
                self.album_editor_hits.push((hit, rect));
            }
        }
        self.library_cards.clear();
        let row_top = panel.y0 + 286.0;
        for (index, item) in self.library_items.iter().take(2).enumerate() {
            let row_y = row_top + index as f64 * 84.0;
            let row = Rect::new(panel.x0 + 16.0, row_y, panel.x1 - 16.0, row_y + 74.0);
            let selected = self.album_editor_members.contains(&item.id);
            scene.fill(
                Fill::NonZero,
                transform,
                if selected {
                    theme.tokens.colors.neutral[3]
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &row,
            );
            scene.stroke(
                &Stroke::new(if selected { 2.0 } else { 1.0 }),
                transform,
                if selected {
                    theme.tokens.colors.accent[6]
                } else {
                    theme.border_standard
                },
                None,
                &row,
            );
            let toggle = Rect::new(row.x0 + 10.0, row.y0 + 18.0, row.x0 + 48.0, row.y0 + 56.0);
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                theme.border_standard,
                None,
                &toggle,
            );
            if selected {
                draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(toggle.center(), (20.0, 20.0)),
                    Icon::Check,
                    theme.tokens.colors.accent[6],
                    1.8,
                );
            }
            text_renderer.draw_text_weighted(
                scene,
                &item.name,
                11.0,
                theme.text_primary,
                transform * Affine::translate((row.x0 + 60.0, row.y0 + 12.0)),
                Some((row.width() - 72.0) as f32),
                TextWeight::Medium,
            );
            self.library_cards
                .push((item.id.clone(), row, item.favorite));
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
        if theme.text_scale > 1.25 {
            self.draw_large_text_metadata_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 420.0 };
        let scale = f64::from(theme.text_scale);
        // Keep each field's scaled label, input, and its own focus ring on a
        // separate line. The former fixed 66px rhythm made 200% text paint
        // through the next field and the footer actions.
        let panel_h = (408.0 * scale).min((bounds.height() - 24.0).max(0.0));
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 12.0
            },
            if narrow {
                (bounds.y1 - panel_h - 8.0).max(bounds.y0 + 8.0)
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
                (bounds.y0 + 12.0 + panel_h).min(bounds.y1 - 8.0)
            },
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
        text_renderer.draw_text_weighted(
            scene,
            "Photo details",
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0 * scale)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Shared labels only — no automatic people identification",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 48.0 * scale)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        let fields = [
            ("Folder", &mut self.metadata_folder_input),
            ("People (comma-separated)", &mut self.metadata_people_input),
            ("Tags (comma-separated)", &mut self.metadata_tags_input),
            ("Captured", &mut self.metadata_captured_at_input),
        ];
        for (index, (label, input)) in fields.into_iter().enumerate() {
            // Leave a full scaled line after the privacy subtitle.  At 200%
            // the sentence wraps, so the old 70px origin put “Folder” on its
            // second line even though the field rectangles themselves fit.
            let y = panel.y0 + 90.0 * scale + index as f64 * 60.0 * scale;
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                theme.text_dim,
                transform * Affine::translate((panel.x0 + 16.0, y)),
                Some((panel.width() - 32.0) as f32),
                TextWeight::Regular,
            );
            input.apply_layout_external(Rect::new(
                panel.x0 + 16.0,
                y + 17.0 * scale,
                panel.x1 - 16.0,
                y + 55.0 * scale,
            ));
            input.draw(scene, transform, text_renderer, theme, presence);
        }
        let y = panel.y1 - 64.0 * scale;
        let cancel = Rect::new(
            panel.x0 + 16.0,
            y,
            panel.x0 + 112.0 * scale,
            y + 44.0 * scale,
        );
        let save = Rect::new(
            panel.x1 - 96.0 * scale,
            y,
            panel.x1 - 16.0,
            y + 44.0 * scale,
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
            text_renderer.draw_text_weighted(
                scene,
                label,
                13.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 14.0 * scale)),
                Some((rect.width() - 16.0) as f32),
                TextWeight::Medium,
            );
            self.metadata_editor_hits.push((hit, rect));
        }
    }

    /// Reflow the details form for enlarged text. Font scaling belongs to the
    /// text renderer; multiplying every desktop offset by the same factor
    /// produced an 816 px form in a 728 px phone canvas and hid Captured under
    /// the footer. This rhythm reserves a complete row for every label/input
    /// and anchors the actions only after those rows.
    fn draw_large_text_metadata_editor(
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
        let panel_w = if narrow {
            bounds.width() - 16.0
        } else {
            480.0_f64.min(bounds.width() - 24.0)
        };
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 12.0
            },
            bounds.y0 + 8.0,
            bounds.x1 - if narrow { 8.0 } else { 12.0 },
            bounds.y1 - 8.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &panel,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Photo details",
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "No automatic face matching",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 62.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );

        self.draw_large_metadata_fields(scene, transform, text_renderer, theme, presence, panel);
        self.draw_large_metadata_actions(scene, transform, text_renderer, theme, panel);
    }

    fn draw_large_metadata_fields(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        panel: Rect,
    ) {
        let available_for_fields = (panel.height() - 178.0).max(288.0);
        let row_h = (available_for_fields / 4.0).clamp(72.0, 92.0);
        let fields = [
            ("Folder", &mut self.metadata_folder_input),
            ("People (comma-separated)", &mut self.metadata_people_input),
            ("Tags (comma-separated)", &mut self.metadata_tags_input),
            ("Captured", &mut self.metadata_captured_at_input),
        ];
        for (index, (label, input)) in fields.into_iter().enumerate() {
            let y = panel.y0 + 106.0 + index as f64 * row_h;
            text_renderer.draw_text_weighted(
                scene,
                label,
                11.0,
                theme.text_dim,
                transform * Affine::translate((panel.x0 + 16.0, y)),
                Some((panel.width() - 32.0) as f32),
                TextWeight::Regular,
            );
            input.apply_layout_external(Rect::new(
                panel.x0 + 16.0,
                y + 24.0,
                panel.x1 - 16.0,
                (y + row_h - 4.0).max(y + 68.0),
            ));
            input.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_large_metadata_actions(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        use vello::peniko::Fill;

        let action_y = panel.y1 - 64.0;
        let action_w = ((panel.width() - 40.0) * 0.5).max(96.0);
        let cancel = Rect::new(
            panel.x0 + 16.0,
            action_y,
            panel.x0 + 16.0 + action_w,
            panel.y1 - 12.0,
        );
        let save = Rect::new(
            panel.x1 - 16.0 - action_w,
            action_y,
            panel.x1 - 16.0,
            panel.y1 - 12.0,
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
                Fill::NonZero,
                transform,
                if focused { theme.bg_hover } else { color },
                None,
                &rect,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 12.0, rect.y0 + 13.0)),
                Some((rect.width() - 24.0) as f32),
                TextWeight::Medium,
            );
            self.metadata_editor_hits.push((hit, rect));
        }
    }
}

fn wrap_words(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let separator = usize::from(!line.is_empty());
        if !line.is_empty()
            && line
                .chars()
                .count()
                .saturating_add(separator)
                .saturating_add(word.chars().count())
                > max_chars
        {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn metadata_filter_id(folder: Option<&str>, person: Option<&str>, tag: Option<&str>) -> String {
    if let Some(folder) = folder {
        crate::actions::folder_resource_id(folder)
    } else if let Some(person) = person {
        crate::actions::person_resource_id(person)
    } else if let Some(tag) = tag {
        crate::actions::tag_resource_id(tag)
    } else {
        "all".to_owned()
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the helper keeps one centered text paint call explicit at each compact empty-state line"
)]
fn draw_centered_text(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    color: vello::peniko::Color,
    center_x: f64,
    y: f64,
    max_width: f64,
    size: f32,
    weight: TextWeight,
    text: &str,
) {
    let (measured, _) = text_renderer.measure_weighted(text, size, None, weight);
    let width = f64::from(measured).min(max_width);
    text_renderer.draw_text_weighted(
        scene,
        text,
        size,
        color,
        transform * Affine::translate((center_x - width * 0.5, y)),
        Some(max_width as f32),
        weight,
    );
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
        item_width(face, &label)
    };
    Button::new(key, shown, None)
        .with_icon(icon)
        .with_size(width, BTN_H)
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
        .with_size(92.0, 44.0)
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

fn build_library_header_button(id: &str, label: &str) -> Button {
    Button::new(id, label, None)
        .with_size(142.0, 44.0)
        .tooltip(label)
        .with_accessible_name(label)
}

fn build_library_album_buttons() -> Vec<(AlbumEditorHit, Button)> {
    [
        (AlbumEditorHit::New, "photos-library-new-album", "New album"),
        (
            AlbumEditorHit::Edit,
            "photos-library-edit-album",
            "Edit album",
        ),
    ]
    .into_iter()
    .map(|(hit, id, label)| (hit, build_library_header_button(id, label)))
    .collect()
}

/// Shared retained controls placed over the custom timeline geometry.  The
/// timeline owns placement, while Button owns its visual states, names and
/// minimum target treatment.
fn build_timeline_density_buttons(
    pending: &Rc<Cell<Option<TimelineHit>>>,
) -> Vec<(TimelineHit, Button)> {
    [
        (
            TimelineHit::ZoomOut,
            "photos-timeline-density-out",
            "Show fewer photographs",
            Icon::Minimize2,
        ),
        (
            TimelineHit::ZoomIn,
            "photos-timeline-density-in",
            "Show more photographs",
            Icon::Plus,
        ),
    ]
    .into_iter()
    .map(|(hit, id, name, icon)| {
        let slot = pending.clone();
        (
            hit,
            Button::new(id, "", None)
                .with_icon(icon)
                .with_size(44.0, 44.0)
                .tooltip(name)
                .with_accessible_name(name)
                .on_effect(move || slot.set(Some(hit))),
        )
    })
    .collect()
}

/// Wrap an RGBA buffer as the image type the scene draws.
fn to_peniko(img: &Rgba) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(img.bytes.clone())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: img.w,
        height: img.h,
    }
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

/// Turn the portable capture value into a calm timeline label without parsing
/// it into the viewer's timezone. A collaborator who only supplied a date
/// sees exactly that date; an ISO timestamp naturally groups with its day.
/// The stored value remains untouched for ordering, search, and resources.
fn timeline_date_label(captured_at: &str) -> String {
    let day = capture_day(captured_at);
    let Some((year, rest)) = day.split_once('-') else {
        return day;
    };
    let Some((month, day_of_month)) = rest.split_once('-') else {
        return day;
    };
    let month = match month {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => return day,
    };
    if day_of_month.len() == 2 && day_of_month.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{month} {day_of_month}, {year}")
    } else {
        day
    }
}

/// Preserve a capture day as the timeline's chronological identity. Display
/// formatting must never become the sort/group key: doing so would collapse
/// every "Jun" from different years into one ambiguous bucket.
fn capture_day(captured_at: &str) -> String {
    captured_at
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("Undated")
        .to_owned()
}

/// Materialise lightweight chronological boundaries for the shared index.
/// The collection stays virtual: this walks metadata already projected during
/// `sync`, creates at most one marker per visible capture day, and the widget
/// later paints/exports only a bounded sample.  Newest-first remains position
/// zero so the date index and the timeline use the same direction.
fn timeline_index_markers(items: &[LibraryItem], columns: usize) -> Vec<DateIndexMarker> {
    let denominator = items.len().saturating_sub(1).max(1) as f64;
    let columns = columns.max(1);
    let mut markers = Vec::new();
    let mut previous_day = "";
    let mut previous_year = "";
    for (index, item) in items.iter().enumerate() {
        if item.captured_at == previous_day {
            continue;
        }
        let year = item.captured_at.get(..4).unwrap_or_default();
        let kind = if year.len() == 4 && year != previous_year {
            previous_year = year;
            DateIndexMarkerKind::Major
        } else {
            DateIndexMarkerKind::Minor
        };
        let row = index / columns;
        markers.push(DateIndexMarker::new(
            format!("{}:{}", item.captured_at, row),
            timeline_date_label(&item.captured_at),
            index as f64 / denominator,
            kind,
        ));
        previous_day = &item.captured_at;
    }
    markers
}

/// Read at most the encoded-image budget plus one sentinel byte. A hostile
/// file can otherwise exhaust memory before the decoder gets a chance to
/// enforce its decoded-pixel limit.
fn bounded_file_bytes(path: &str) -> Option<Vec<u8>> {
    pixels::read_bounded(std::fs::File::open(path).ok()?, pixels::MAX_ENCODED_BYTES)
}

fn is_importable_photo(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp"
            )
        })
}

impl PhotosApp {
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
        let ids: Vec<_> = sources.iter().map(|photo| photo.id.clone()).collect();
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
            !self.library_thumbnails.contains_key(&photo.id)
                && !self.thumbnail_unavailable.contains(&photo.id)
                && !self.thumbnail_queued.contains(&photo.id)
        }) else {
            return;
        };
        let id = photo.id.clone();
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
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::PHOTOS_OPERATION_TYPE]
    }

    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            crate::actions::PhotosOperation::View => {
                self.library_mode = false;
                self.album_editor = false;
                self.metadata_editor = false;
            }
            crate::actions::PhotosOperation::ViewLibrary => self.show_library(),
            crate::actions::PhotosOperation::ViewPeople => {
                self.show_library();
                self.people_mode = true;
                self.library_destination = LibraryDestination::People;
                self.notice = Some(
                    "People recognition is local and opt-in; install a verified face model to begin"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Settings › Models to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            crate::actions::PhotosOperation::ViewFavorites => self.view_favorites(doc)?,
            crate::actions::PhotosOperation::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.notice = Some("Showing offline Places".to_owned());
            }
            crate::actions::PhotosOperation::ViewPlace { place_key } => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                if let Some(photo) = PhotosDoc::library(doc).photos.iter().find(|photo| {
                    photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == place_key)
                }) && let Some(location) = &photo.location
                {
                    self.places_viewport.latitude = location.latitude;
                    self.places_viewport.longitude = location.longitude;
                    self.places_viewport.zoom = 12.0;
                }
                self.notice = Some("Showing selected place offline".to_owned());
            }
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
            crate::actions::PhotosOperation::ViewObject { object } => {
                self.view_analysis_filter(doc, object, false)?;
            }
            crate::actions::PhotosOperation::ViewPet { pet } => {
                self.view_analysis_filter(doc, pet, true)?;
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
        if let crate::actions::PhotosOperation::Open { path: Some(file) } = operation {
            self.pending_resource_open = Some((invocation, file));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if self.picker.is_some()
            || self.directory_picker.is_some()
            || self.directory_import.is_some()
        {
            self.resource_invocation = Some(invocation);
            if let Some(import) = self.directory_import.as_mut() {
                import.invocation = Some(invocation);
            }
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    fn start_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if let crate::actions::PhotosOperation::AnalyzeText {
            resource_id,
            language,
        } = operation
        {
            let photo = PhotosDoc::photo(doc, &resource_id)
                .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
            let sender = self
                .analysis_tx
                .as_ref()
                .ok_or_else(|| "local text recognition is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id: resource_id,
                    content: photo.image.content.clone(),
                    image: photo.image,
                    kind: AnalysisJobKind::Ocr { language },
                })
                .map_err(|_| {
                    "text recognition is busy; wait for the current photos to finish".to_owned()
                })?;
            self.active_analysis.insert(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if let crate::actions::PhotosOperation::AnalyzeImage { resource_id } = operation {
            let photo = PhotosDoc::photo(doc, &resource_id)
                .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
            let capability = tilo_vision::discover_local_capability(
                &agent_provider_config::ProviderConfig::load(),
            )
            .ok_or_else(|| {
                "No verified local image-analysis model is ready. Install and start one in Settings."
                    .to_owned()
            })?;
            let sender = self
                .analysis_tx
                .as_ref()
                .ok_or_else(|| "local image analysis is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id: resource_id,
                    content: photo.image.content.clone(),
                    image: photo.image,
                    kind: AnalysisJobKind::Vision { capability },
                })
                .map_err(|_| {
                    "local image analysis is busy; wait for the current photos to finish".to_owned()
                })?;
            self.active_analysis.insert(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if matches!(
            operation,
            crate::actions::PhotosOperation::AnalyzePeopleLibrary
        ) {
            if !self.people_controller.privacy().recognition_enabled {
                return Err("People recognition is off on this device. Enable it in People before scanning local photographs.".to_owned());
            }
            let config = agent_provider_config::ProviderConfig::load();
            self.people_controller.discover_face_capability(&config);
            let capability =
                tilo_vision::discover_local_face_capability(&config).ok_or_else(|| {
                    "No verified local face model is ready. Install and start one in Settings."
                        .to_owned()
                })?;
            let library = PhotosDoc::library(doc);
            let locally_available: Vec<_> = library
                .photos
                .into_iter()
                .filter(|photo| crate::doc::bytes_of(&photo.image).is_some())
                .map(|photo| (photo.id, photo.image))
                .collect();
            if locally_available.is_empty() {
                return Err(
                    "No locally available photographs are ready for People recognition.".to_owned(),
                );
            }
            let start = self.people_scan_progress.next_offset % locally_available.len();
            let photos: Vec<_> = locally_available
                .iter()
                .cycle()
                .skip(start)
                .take(tilo_vision::MAX_FACE_BATCH.min(locally_available.len()))
                .cloned()
                .collect();
            self.people_scan_progress.next_offset =
                start.saturating_add(photos.len()) % locally_available.len();
            self.people_scan_progress.last_library_size = locally_available.len();
            self.people_scan_progress.status = "running".to_owned();
            self.people_scan_progress.last_error = None;
            self.people_scan_progress.save();
            let Some((_, first)) = photos.first() else {
                return Err(
                    "No locally available photographs are ready for People recognition.".to_owned(),
                );
            };
            let sender = self.analysis_tx.as_ref().ok_or_else(|| {
                "local People recognition is unavailable on this device".to_owned()
            })?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id: "photos.people".to_owned(),
                    content: None,
                    image: first.clone(),
                    kind: AnalysisJobKind::People {
                        capability,
                        images: photos,
                    },
                })
                .map_err(|_| {
                    "local image analysis is busy; wait for the current photos to finish".to_owned()
                })?;
            self.active_analysis.insert(invocation);
            self.active_people_scans.insert(invocation);
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }

        let crate::actions::PhotosOperation::ImportCameraMedia { media_id, name } = operation
        else {
            return self.start_resource_operation(invocation, kind, type_id, payload, doc, path);
        };

        // This is deliberately a compound admission boundary: verify the
        // portable Camera identity and bytes before committing the one
        // idempotent Photos library/selection update.  A missing local blob
        // remains a refusal instead of becoming a path-only phantom entry.
        let photo = crate::camera_import_photo(&media_id, &name)?;
        let id = photo.id.clone();
        let action = crate::actions::PhotosAction::ImportPhoto { photo };
        self.actions
            .try_dispatch_with_context(
                doc,
                crate::actions::PHOTOS_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("org.tilo.Photos", "Photos", &action),
            )
            .map_err(|error| error.to_string())?;
        self.notice = Some("Imported Camera photo".to_owned());
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput {
                value: Some(serde_json::Value::String(id.clone())),
                resources: vec![id],
            },
        )))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested && self.active_analysis.remove(&invocation) {
            self.cancelled_analysis.insert(invocation);
            if self.active_people_scans.remove(&invocation) {
                self.people_scan_progress.status = "cancelled".to_owned();
                self.people_scan_progress.save();
            }
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("local image analysis was cancelled".into()),
            });
        }
        if cancellation_requested
            && self
                .pending_resource_open
                .as_ref()
                .is_some_and(|(pending, _)| *pending == invocation)
        {
            self.pending_resource_open = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("photo opening was cancelled".into()),
            });
        }
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            // Dropping the receiver closes this app's portal request channel;
            // a late worker response can no longer open a file after the
            // caller has cancelled the action.
            self.picker = None;
            self.directory_picker = None;
            if let Some(import) = self.directory_import.take() {
                import.cancel.store(true, Ordering::Relaxed);
            }
            self.resource_invocation = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("photo selection was cancelled".into()),
            });
        }
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
    fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
        PhotosDoc::seed(doc);
        self.drain_analysis_results(doc);
        if let Some((invocation, path)) = self.pending_resource_open.take() {
            self.open_path(doc, &path);
            self.resource_result = Some(if self.open_image_path().is_some() {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.open_failed".into(),
                        message: tr("photos-open-failed"),
                        details: None,
                        retryable: false,
                    },
                }
            });
        }
        self.drain_picker(doc);
        self.drain_directory_import(doc);
        self.refresh(doc);
        self.library_search.sync(doc, path);
        if let Some(query) = self.library_search.take_dirty() {
            self.library_query = query;
            self.timeline_scroll_row = 0.0;
        }

        // An address can arrive before its bytes. Do not offer Copy, Save or
        // a blind crop against an empty canvas; the durable resource remains
        // available to a headless collaborator that deliberately chooses it.
        let has_image = self.has_image();
        let edited = !PhotosDoc::edits(doc).is_identity();
        for (hit, button) in &mut self.buttons {
            button.set_enabled(Self::enabled(*hit, has_image, edited));
        }
        let heads = doc.get_heads();
        let document_changed = heads != self.search_heads;
        if document_changed {
            self.search_refresh_error = match self.refresh_search_provider(doc) {
                Ok(()) => None,
                Err(error) => Some(format!("Photo search kept its previous index: {error}")),
            };
            self.thumbnail_unavailable.clear();
            self.search_heads = heads;
        }
        self.status = self.status_text(doc);
        let library = PhotosDoc::library(doc);
        self.selection_all_favorite = !self.selected_photo_ids.is_empty()
            && self
                .selected_photo_ids
                .iter()
                .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
        if self
            .selection_destination_album
            .as_ref()
            .is_some_and(|id| library.album(id).is_none())
        {
            self.selection_destination_album = None;
            self.selection_album_chooser = false;
            self.notice = Some("The selected destination album was removed elsewhere".to_owned());
        }
        let membership = self.active_album.as_ref().and_then(|id| {
            library
                .albums
                .iter()
                .find(|album| &album.id == id)
                .map(|album| &album.photo_ids)
        });
        let query = self.library_query.trim().to_lowercase();
        let matches_filter = |photo: &LibraryPhoto| {
            let query_matches = query.is_empty()
                || format!(
                    "{} {} {} {} {} {} {}",
                    photo.image.name,
                    photo.folder.as_deref().unwrap_or_default(),
                    photo.people.join(" "),
                    photo.tags.join(" "),
                    photo.captured_at.as_deref().unwrap_or_default(),
                    photo.metadata_summary(),
                    photo
                        .analysis
                        .as_ref()
                        .map_or_else(String::new, crate::model::ImageAnalysis::searchable_text),
                )
                .to_lowercase()
                .contains(&query);
            membership.is_none_or(|ids| ids.contains(&photo.id))
                && query_matches
                && (!self.active_favorites || photo.favorite)
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
                && self.active_object.as_ref().is_none_or(|object| {
                    photo
                        .analysis
                        .as_ref()
                        .is_some_and(|analysis| analysis.objects.contains(object))
                })
                && self.active_pet.as_ref().is_none_or(|pet| {
                    photo
                        .analysis
                        .as_ref()
                        .is_some_and(|analysis| analysis.pets.contains(pet))
                })
        };
        self.library_total = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .count();
        self.library_favorite_count = library.photos.iter().filter(|photo| photo.favorite).count();
        self.library_offset = self
            .library_offset
            .min(self.library_total.saturating_sub(1));
        let timeline_columns = timeline_column_count(
            self.canvas_rect.width(),
            self.timeline_density,
            self.large_text_toolbar,
        );
        self.timeline_last_columns = timeline_columns;
        self.timeline_scroll_row = self
            .timeline_scroll_row
            .min(self.library_total.saturating_sub(1) as f64 / timeline_columns.max(1) as f64);
        let browse_offset = if self.album_editor {
            self.library_offset
        } else {
            (self.timeline_scroll_row.floor() as usize).saturating_mul(timeline_columns)
        };
        let mut timeline_items: Vec<_> = library
            .photos
            .iter()
            .filter(|photo| matches_filter(photo))
            .map(|photo| LibraryItem {
                id: photo.id.clone(),
                name: photo.image.name.clone(),
                favorite: photo.favorite,
                metadata: photo.metadata_card_summary(),
                captured_at: photo
                    .captured_at
                    .as_deref()
                    .map_or_else(|| "Undated".to_owned(), capture_day),
            })
            .collect();
        // A timeline reads newest-first.  The id tie-breaker makes peers with
        // the same capture-day stable without making the date a mutable sort
        // key in the shared document.
        timeline_items.sort_by(|left, right| {
            let left_undated = left.captured_at == "Undated";
            let right_undated = right.captured_at == "Undated";
            left_undated
                .cmp(&right_undated)
                .then_with(|| right.captured_at.cmp(&left.captured_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.timeline_index_markers = timeline_index_markers(&timeline_items, timeline_columns);
        self.library_items = timeline_items
            .into_iter()
            .skip(browse_offset)
            .take(120)
            .collect();
        self.places_items = library
            .photos
            .iter()
            .filter(|photo| {
                photo
                    .location
                    .as_ref()
                    .is_some_and(crate::model::PhotoLocation::is_valid)
            })
            .cloned()
            .map(|photo| PlaceItem { photo })
            .collect();
        self.library_albums = library
            .albums
            .iter()
            .take(24)
            .map(|album| (album.id.clone(), album.name.clone(), album.photo_ids.len()))
            .collect();
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        let mut tags = std::collections::BTreeMap::<String, usize>::new();
        for photo in &library.photos {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in &photo.people {
                *people.entry(person.clone()).or_default() += 1;
            }
            for tag in &photo.tags {
                *tags.entry(tag.clone()).or_default() += 1;
            }
        }
        self.library_folders = folders.into_iter().take(6).collect();
        self.library_people = people.into_iter().take(6).collect();
        self.library_person_covers.clear();
        for (person, _) in &self.library_people {
            if let Some(photo) = library
                .photos
                .iter()
                .find(|photo| photo.people.iter().any(|label| label == person))
            {
                self.library_person_covers
                    .insert(person.clone(), photo.id.clone());
            }
        }
        self.library_tags = tags.into_iter().take(6).collect();
        // Request exactly the sorted, virtualised timeline slice, not the
        // document's original insertion order.  Otherwise a visible newest
        // photo can remain a placeholder while an off-screen old one decodes.
        let mut thumbnail_sources: Vec<_> = self
            .library_items
            .iter()
            .take(24)
            .filter_map(|item| library.photo(&item.id).cloned())
            .collect();
        if self.library_destination == LibraryDestination::Places {
            thumbnail_sources = self
                .places_items
                .iter()
                .take(24)
                .map(|item| item.photo.clone())
                .collect();
        } else {
            for cover in self.library_person_covers.values() {
                if thumbnail_sources.iter().all(|photo| &photo.id != cover)
                    && let Some(photo) = library.photo(cover).cloned()
                {
                    thumbnail_sources.push(photo);
                }
            }
            thumbnail_sources.truncate(24);
        }
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
        self.refit_toolbar(self.rect.width() as f32);

        let bar = self.build_toolbar_node(taffy, theme, node_map);
        let search = self.build_library_search_node(taffy, theme, node_map);
        let canvas = self.build_canvas_node(taffy, theme, node_map);
        let status = self.build_status_node(taffy, theme);

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
                &[bar, search, canvas, status],
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

        if let Some(&search) = children.get(1) {
            let sl = taffy.layout(search).unwrap();
            let search_origin =
                Point::new(x + f64::from(sl.location.x), y + f64::from(sl.location.y));
            if let Some(&field) = taffy.children(search).unwrap_or_default().first() {
                self.library_search
                    .apply_layout(taffy, field, search_origin);
            }
        }
        if let Some(&canvas) = children.get(2) {
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
        if let Some(&status) = children.get(3) {
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
            self.library_search
                .draw(scene, transform, text_renderer, theme, presence);
            if self.library_destination == LibraryDestination::People || self.people_mode {
                self.draw_people(scene, transform, text_renderer, theme);
            } else if self.library_destination == LibraryDestination::Places {
                self.draw_places(scene, transform, text_renderer, theme, presence);
            } else {
                self.draw_library(scene, transform, text_renderer, theme, presence);
            }
        } else if let Some(v) = self.viewer.as_mut() {
            v.draw(scene, transform, text_renderer, theme, presence);
        } else {
            let (title, body) = self.empty_state_text();
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
        }
        // This is deliberately after the canvas/library so the compact menu
        // is a real popup rather than a panel hidden behind the grid.
        self.draw_compact_more(scene, transform, text_renderer, theme);

        if !self.status.is_empty() {
            let inner = self.status_rect;
            text_renderer.draw_text_weighted(
                scene,
                &self.status,
                13.0,
                theme.text_primary,
                transform * Affine::translate((inner.x0 + f64::from(PAD), inner.y0 + 4.0)),
                Some((inner.width() - 2.0 * f64::from(PAD)) as f32),
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
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            let next = Point::new(position.x / scale_factor, position.y / scale_factor);
            if self.library_mode
                && self.library_destination == LibraryDestination::Places
                && let Some(previous) = self.places_drag_origin.replace(next)
            {
                self.places_viewport = self.places_viewport.panned(
                    next.x - previous.x,
                    next.y - previous.y,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                );
            }
            self.pointer = next;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && self.places_map_bounds.contains(self.pointer)
            && !self
                .places_hits
                .iter()
                .any(|(_, rect)| rect.contains(self.pointer))
        {
            self.places_drag_origin = Some(self.pointer);
            *handled = true;
            return;
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            self.places_drag_origin = None;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                winit::event::MouseScrollDelta::PixelDelta(point) => point.y / 80.0,
            };
            self.places_viewport.zoom = (self.places_viewport.zoom + amount * 0.25)
                .clamp(crate::places::MIN_ZOOM, crate::places::MAX_ZOOM);
            *handled = true;
            return;
        }
        if self.library_mode
            && let Some(hit) = self.timeline_density_pending.take()
        {
            self.adjust_timeline_density(matches!(hit, TimelineHit::ZoomIn));
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
            && let Some((hit, _)) = self
                .places_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            match hit {
                PlacesHit::Timeline => {
                    self.people_mode = false;
                    self.library_destination = LibraryDestination::Timeline;
                }
                PlacesHit::People => {
                    self.people_mode = true;
                    self.library_destination = LibraryDestination::People;
                }
                PlacesHit::Places => {
                    self.people_mode = false;
                    self.library_destination = LibraryDestination::Places;
                }
                PlacesHit::ToggleList => self.places_list_mode = !self.places_list_mode,
                PlacesHit::ZoomIn => {
                    self.places_viewport.zoom =
                        (self.places_viewport.zoom + 1.0).min(crate::places::MAX_ZOOM)
                }
                PlacesHit::ZoomOut => {
                    self.places_viewport.zoom =
                        (self.places_viewport.zoom - 1.0).max(crate::places::MIN_ZOOM)
                }
                PlacesHit::Marker(id) => {
                    let _ = self.actions.try_dispatch_with_context(
                        doc,
                        crate::actions::PHOTOS_ACTION_TYPE,
                        &crate::actions::PhotosAction::SelectPhoto {
                            photo_id: id.clone(),
                        },
                        tilo_actions::ActionContext::for_action(
                            "org.tilo.Photos",
                            "Photos",
                            &crate::actions::PhotosAction::SelectPhoto { photo_id: id },
                        ),
                    );
                    self.library_mode = false;
                }
            }
            *handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::ArrowLeft) => {
                    self.places_viewport = self.places_viewport.panned(
                        44.0,
                        0.0,
                        self.canvas_rect.width(),
                        self.canvas_rect.height(),
                    )
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.places_viewport = self.places_viewport.panned(
                        -44.0,
                        0.0,
                        self.canvas_rect.width(),
                        self.canvas_rect.height(),
                    )
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.places_viewport = self.places_viewport.panned(
                        0.0,
                        44.0,
                        self.canvas_rect.width(),
                        self.canvas_rect.height(),
                    )
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.places_viewport = self.places_viewport.panned(
                        0.0,
                        -44.0,
                        self.canvas_rect.width(),
                        self.canvas_rect.height(),
                    )
                }
                Key::Character(key) if key == "+" || key == "=" => {
                    self.places_viewport.zoom =
                        (self.places_viewport.zoom + 1.0).min(crate::places::MAX_ZOOM)
                }
                Key::Character(key) if key == "-" => {
                    self.places_viewport.zoom =
                        (self.places_viewport.zoom - 1.0).max(crate::places::MIN_ZOOM)
                }
                Key::Character(key) if key.eq_ignore_ascii_case("l") => {
                    self.places_list_mode = !self.places_list_mode
                }
                _ => return,
            }
            *handled = true;
            return;
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

        // The library search is a real shared SearchField, placed directly
        // beneath the toolbar so OCR/descriptions and people/object facets
        // have one obvious entry point instead of another filter-chip row.
        if self.library_mode
            && let Some(&search_wrap) = taffy.children(node_id).unwrap_or_default().get(1)
            && let Some(&search) = taffy.children(search_wrap).unwrap_or_default().first()
        {
            self.library_search.handle_event(
                event,
                scale_factor,
                modifiers,
                doc,
                path,
                handled,
                focus_target,
                search,
                taffy,
            );
            if *handled {
                return;
            }
        }

        // Give the shared chronological index first refusal only while its
        // narrow track/preview lane owns the pointer (or it holds a drag / AT
        // focus). Ordinary scrolling anywhere else remains the timeline's
        // smooth wheel path below.
        if self.library_mode && !self.album_editor && !self.metadata_editor {
            let index_owns_event = self.timeline_index.contains_interaction_point(self.pointer)
                || self.timeline_index.is_dragging()
                || self.timeline_index.is_focused();
            if index_owns_event
                && matches!(
                    event,
                    winit::event::WindowEvent::CursorMoved { .. }
                        | winit::event::WindowEvent::MouseInput { .. }
                        | winit::event::WindowEvent::MouseWheel { .. }
                        | winit::event::WindowEvent::KeyboardInput { .. }
                )
            {
                let was_dragging = self.timeline_index.is_dragging();
                if let Some(intent) = self.timeline_index.handle_event(
                    event,
                    scale_factor,
                    self.timeline_index_metrics,
                    &self.timeline_index_markers,
                ) {
                    let _ = self.apply_timeline_index_intent(intent);
                    *handled = true;
                    return;
                }
                if was_dragging || self.timeline_index.is_dragging() {
                    *handled = true;
                    return;
                }
            }
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

        // Ctrl + wheel is the cross-platform density gesture.  Keep an
        // ordinary wheel for chronological browsing; otherwise a trackpad
        // scroll would unexpectedly rearrange the entire library.
        if self.library_mode
            && !self.album_editor
            && !self.metadata_editor
            && modifiers.state().control_key()
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if amount != 0.0 {
                // Natural scroll semantics: up means see more at once.
                self.adjust_timeline_density(amount < 0.0);
                *handled = true;
                return;
            }
        }

        // Tilo's vendored Wayland winit maps zwp_pointer_gestures_v1 to this
        // same event; macOS/iOS use it natively.  Ctrl-wheel above remains the
        // fallback on compositors that do not advertise pinch gestures.
        if self.library_mode
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::PinchGesture { delta, .. } = event
            && delta.is_finite()
            && *delta != 0.0
        {
            self.adjust_timeline_density(*delta > 0.0);
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
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            if (modifiers.state().control_key() || modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value.eq_ignore_ascii_case("a"))
                && self.selection_mode
            {
                self.select_all_visible();
                *handled = true;
                return;
            }
            if (modifiers.state().control_key() || modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value == " ")
            {
                if self.selection_mode {
                    self.finish_selection();
                } else {
                    self.begin_selection();
                }
                *handled = true;
                return;
            }
            if self.selection_mode && matches!(&key.logical_key, Key::Named(NamedKey::Escape)) {
                self.finish_selection();
                *handled = true;
                return;
            }
            let forward = match &key.logical_key {
                Key::Named(NamedKey::PageDown | NamedKey::ArrowRight) => Some(true),
                Key::Named(NamedKey::PageUp | NamedKey::ArrowLeft) => Some(false),
                _ => None,
            };
            if let Some(forward) = forward {
                if self.album_editor {
                    self.page_library(forward);
                } else {
                    self.scroll_timeline_rows(if forward { 3.0 } else { -3.0 });
                }
                *handled = true;
                return;
            }
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
                .timeline_density_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.adjust_timeline_density(matches!(hit, TimelineHit::ZoomIn));
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
            && let Some((forward, enabled, _)) = self
                .library_page_hits
                .iter()
                .find(|(_, _, rect)| rect.contains(self.pointer))
                .copied()
            && enabled
        {
            self.page_library(forward);
            *handled = true;
            return;
        }

        if self.library_mode
            && self.selection_album_chooser
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .selection_destination_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.selection_destination_album = Some(album_id);
            self.notice =
                Some("Destination album selected; choose Use album to continue".to_owned());
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
            && let Some((hit, _)) = self
                .selection_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            self.handle_selection_hit(hit, doc);
            *handled = true;
            return;
        }

        if self.library_mode
            && self.selection_album_chooser
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
        {
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
            self.active_favorites = false;
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
            && let Some(card) = self
                .metadata_filter_cards
                .iter()
                .find(|card| card.bounds.contains(self.pointer))
                .cloned()
        {
            if card.favorites {
                let _ = self.view_favorites(doc);
            } else {
                self.active_album = None;
                self.active_folder = card.folder;
                self.active_person = card.person;
                self.active_tag = card.tag;
                self.active_favorites = false;
                self.library_offset = 0;
                self.notice = None;
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
            let rows = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(y.signum()),
                winit::event::MouseScrollDelta::PixelDelta(position) => {
                    (-position.y / 96.0).clamp(-3.0, 3.0)
                }
            };
            self.scroll_timeline_rows(rows);
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
            if self.selection_mode {
                self.toggle_library_selection(&photo_id, modifiers.state().shift_key());
            } else if !self.album_editor
                && self.pointer.y >= card.y1 - 44.0
                && self.pointer.x >= card.x1 - 88.0
            {
                if self.pointer.x >= card.x1 - 44.0 {
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
        if let (Some(v), Some(&canvas)) = (self.viewer.as_mut(), children.get(2)) {
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
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        use tilo_actions::ResourceProvider;
        let mut resources = crate::actions::PhotosResources.resources(doc);
        if let Some(error) = &self.search_refresh_error {
            resources.push(crate::actions::search_status_resource(error));
        }
        resources
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        use tilo_actions::ResourceProvider;
        if id == "photos.search-status" {
            return self
                .search_refresh_error
                .as_deref()
                .map(crate::actions::search_status_resource);
        }
        self.search_provider
            .resource(id)
            .or_else(|| crate::actions::PhotosResources.resource(doc, id))
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
    use tilo_actions::{Action, ResourceProvider};

    fn has_resource_id(resource: &tilo_actions::Resource, id: &str) -> bool {
        resource.id.0 == id
            || resource
                .children
                .iter()
                .any(|child| has_resource_id(child, id))
    }

    #[test]
    fn timeline_geometry_keeps_200_percent_text_legible_and_density_bounded() {
        assert_eq!(timeline_column_count(390.0, 4, false), 2);
        assert_eq!(timeline_column_count(620.0, 4, false), 2);
        assert_eq!(timeline_column_count(390.0, 4, true), 1);
        assert_eq!(timeline_column_count(1_440.0, 0, false), 4);
        assert_eq!(timeline_column_count(1_440.0, 4, false), 6);
        assert_eq!(timeline_column_count(1_440.0, 4, true), 2);
    }

    #[test]
    fn compact_header_rows_are_non_overlapping_at_200_percent() {
        let header =
            compact_library_header_geometry(Rect::new(0.0, 94.0, 390.0, 820.0), true, true);
        let title = Rect::new(12.0, header.title_y, 378.0, header.title_y + 26.0);
        let count = Rect::new(12.0, header.count_y, 192.0, header.count_y + 22.0);
        let albums = Rect::new(12.0, header.albums_y, 378.0, header.albums_y + 44.0);
        let album_actions = header.album_actions;
        assert!(title.y1 <= count.y0);
        assert!(count.y1 <= header.controls.y0);
        assert!(header.controls.y1 <= albums.y0);
        assert!(albums.y1 <= album_actions.y0);
        assert!((album_actions.height() - 44.0).abs() < f64::EPSILON);
        assert!(album_actions.y1 <= header.timeline_y);
        assert!(header.timeline_y > 94.0);
    }

    #[test]
    fn timeline_virtual_window_has_stable_overscan_without_materialising_the_library() {
        assert_eq!(timeline_virtual_range(500, 12, 4, 5), 60..90);
        assert_eq!(timeline_virtual_range(3, 4, 4, 1), 3..3);
    }

    #[test]
    fn density_reanchors_the_same_first_photo() {
        let mut app = PhotosApp::new();
        app.canvas_rect = Rect::new(0.0, 0.0, 1_440.0, 800.0);
        app.timeline_density = 1;
        app.timeline_last_columns = 5;
        app.timeline_scroll_row = 6.0;
        assert!(app.adjust_timeline_density(true));
        assert!(
            (app.timeline_scroll_row * app.timeline_last_columns as f64 - 30.0).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn date_index_uses_timeline_rows_and_preserves_virtual_scroll_anchor() {
        let mut app = PhotosApp::new();
        app.timeline_index_metrics = ScrollMetrics::new(100.0, 10.0, 20.0);
        assert!(app.apply_timeline_index_intent(DateIndexIntent::ScrollTo { position: 0.5 }));
        assert!((app.timeline_scroll_row - 45.0).abs() < f64::EPSILON);
        // A hover preview may redraw a label but must never mutate the scroll
        // anchor or create an accidental shared action.
        assert!(app.apply_timeline_index_intent(DateIndexIntent::PreviewChanged(None)));
        assert!((app.timeline_scroll_row - 45.0).abs() < f64::EPSILON);
    }

    #[test]
    fn date_index_boundaries_are_chronological_and_mark_year_changes() {
        let items = vec![
            LibraryItem {
                id: "new".into(),
                name: "new".into(),
                favorite: false,
                metadata: String::new(),
                captured_at: "2026-02-03".into(),
            },
            LibraryItem {
                id: "same-day".into(),
                name: "same-day".into(),
                favorite: false,
                metadata: String::new(),
                captured_at: "2026-02-03".into(),
            },
            LibraryItem {
                id: "old".into(),
                name: "old".into(),
                favorite: false,
                metadata: String::new(),
                captured_at: "2025-12-31".into(),
            },
        ];
        let markers = timeline_index_markers(&items, 2);
        assert_eq!(markers.len(), 2, "one boundary per capture day");
        assert_eq!(markers[0].kind, DateIndexMarkerKind::Major);
        assert_eq!(markers[1].kind, DateIndexMarkerKind::Major);
        assert!(markers[0].position < markers[1].position);
        assert_eq!(markers[0].label, "Feb 03, 2026");
        assert_eq!(markers[1].label, "Dec 31, 2025");
    }

    #[test]
    fn date_index_labels_keep_non_iso_values_honest() {
        assert_eq!(timeline_date_label("2026-09-18"), "Sep 18, 2026");
        assert_eq!(timeline_date_label("Undated"), "Undated");
        assert_eq!(timeline_date_label("2026-13-01"), "2026-13-01");
        assert_eq!(capture_day("2026-09-18 08:14"), "2026-09-18");
        assert_eq!(capture_day("Jun 2026"), "Jun");
    }

    #[test]
    fn every_local_analysis_kind_uses_the_same_bounded_queue() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        let image = OpenImage::from_path("queue.png", 2, 2);
        for language in ["eng", "deu"] {
            sender
                .try_send(AnalysisJob {
                    invocation: tilo_actions::InvocationId::new(),
                    photo_id: "photos.photo:queue".to_owned(),
                    content: None,
                    image: image.clone(),
                    kind: AnalysisJobKind::Ocr {
                        language: language.to_owned(),
                    },
                })
                .unwrap();
        }
        assert!(
            sender
                .try_send(AnalysisJob {
                    invocation: tilo_actions::InvocationId::new(),
                    photo_id: "photos.photo:queue".to_owned(),
                    content: None,
                    image,
                    kind: AnalysisJobKind::Ocr {
                        language: "fra".to_owned(),
                    },
                })
                .is_err()
        );
    }

    #[test]
    fn cancelled_or_stale_analysis_can_never_persist() {
        let mut app = PhotosApp::new();
        let (sender, receiver) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        app.analysis_rx = receiver;
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);

        let cancelled = tilo_actions::InvocationId::new();
        app.cancelled_analysis.insert(cancelled);
        sender
            .send(AnalysisResult {
                invocation: cancelled,
                photo_id: "photos.photo:missing".to_owned(),
                content: None,
                result: AnalysisOutcome::Image(Ok(crate::model::ImageAnalysis {
                    ocr_text: "must not persist".to_owned(),
                    ..crate::model::ImageAnalysis::default()
                })),
            })
            .unwrap();
        app.drain_analysis_results(&mut doc);
        assert!(app.resource_result.is_none());

        let stale = tilo_actions::InvocationId::new();
        sender
            .send(AnalysisResult {
                invocation: stale,
                photo_id: "photos.photo:missing".to_owned(),
                content: None,
                result: AnalysisOutcome::Image(Ok(crate::model::ImageAnalysis {
                    ocr_text: "also must not persist".to_owned(),
                    ..crate::model::ImageAnalysis::default()
                })),
            })
            .unwrap();
        app.drain_analysis_results(&mut doc);
        assert!(matches!(
            app.resource_result,
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "photos.analysis_stale"
        ));
        assert!(PhotosDoc::library(&doc).photos.is_empty());
    }

    #[test]
    fn pinch_and_ctrl_wheel_use_the_same_density_event_path() {
        use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};
        use winit::keyboard::ModifiersState;

        let mut app = PhotosApp::new();
        app.library_mode = true;
        app.canvas_rect = Rect::new(0.0, 48.0, 1_280.0, 800.0);
        let mut doc = AutoCommit::new();
        let mut tree = TaffyTree::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut focus = None;
        let mut handled = false;
        let no_modifiers = winit::event::Modifiers::default();
        app.handle_event(
            &WindowEvent::PinchGesture {
                device_id: DeviceId::dummy(),
                delta: 0.2,
                phase: TouchPhase::Moved,
            },
            1.0,
            &no_modifiers,
            &mut doc,
            &ROOT,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
        assert!(handled);
        let after_pinch = app.timeline_density;

        handled = false;
        let ctrl = winit::event::Modifiers::from(ModifiersState::CONTROL);
        app.handle_event(
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: TouchPhase::Moved,
            },
            1.0,
            &ctrl,
            &mut doc,
            &ROOT,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
        assert!(handled);
        assert_eq!(app.timeline_density, after_pinch + 1);
    }

    #[test]
    fn people_review_commits_only_an_explicit_membership_action() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(
            OpenImage::from_path("/photos/person.png", 20, 10)
                .with_content(qdoc::blobstore::hash_bytes(b"people-review")),
        );
        let photo_id = photo.id.clone();
        crate::actions::PhotosAction::ImportPhoto { photo }
            .apply(&mut doc)
            .unwrap();
        assert!(app.people_controller_mut().add_cluster(
            "manual-person",
            [photo_id.clone()],
            crate::people::ProposalOrigin::ImportedOrManual,
        ));
        assert_eq!(
            app.people_controller_mut()
                .confirm_suggestion("manual-person", "Sam"),
            crate::people::PeopleChange::Updated
        );
        assert!(app.approve_people_cluster(&mut doc, "manual-person"));
        assert_eq!(PhotosDoc::photo(&doc, &photo_id).unwrap().people, ["Sam"]);
        assert_eq!(
            app.people_controller()
                .cluster("manual-person")
                .unwrap()
                .origin,
            crate::people::ProposalOrigin::ImportedOrManual
        );
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
        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:trip".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::Effect,
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

        let stale = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:gone".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::Effect,
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
    fn opening_favorites_uses_a_local_view_and_refuses_an_empty_stale_collection() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut favorite =
            LibraryPhoto::from_image(OpenImage::from_path("/photos/favorite.png", 20, 10));
        favorite.favorite = true;
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![favorite],
                albums: Vec::new(),
            },
        );
        let before = PhotosDoc::library(&doc);
        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewFavorites,
        )
        .expect("favorite view operation encodes");
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::View,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
                &doc,
                &ROOT,
            )
            .expect("favorite collection is present")
        );
        assert!(app.library_mode);
        assert!(app.active_favorites);
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "opening a view is not a write"
        );

        PhotosDoc::set_library(&mut doc, &crate::model::PhotoLibrary::default());
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                tilo_actions::OperationKind::View,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
                &doc,
                &ROOT,
            )
            .is_err()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end parity test deliberately compares favorite and membership envelopes across UI, AT, and resource routes"
    )]
    fn selection_is_local_but_its_favorite_and_album_results_are_canonical_actions() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let first = LibraryPhoto::from_image(OpenImage::from_path("/photos/first.png", 20, 10));
        let second = LibraryPhoto::from_image(OpenImage::from_path("/photos/second.png", 20, 10));
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![first.clone(), second.clone()],
                albums: vec![Album {
                    id: "photos.album:trip".into(),
                    name: "Trip".into(),
                    photo_ids: Vec::new(),
                }],
            },
        );
        app.library_cards = vec![
            (first.id.clone(), Rect::new(0.0, 0.0, 44.0, 44.0), false),
            (second.id.clone(), Rect::new(48.0, 0.0, 92.0, 44.0), false),
        ];
        app.library_albums = vec![("photos.album:trip".into(), "Trip".into(), 0)];
        app.begin_selection();
        app.toggle_library_selection(&first.id, false);
        app.toggle_library_selection(&second.id, true);
        assert_eq!(
            app.selected_photo_ids.len(),
            2,
            "range selects visible cards"
        );
        app.set_selected_favorite(&mut doc, true);
        assert!(
            PhotosDoc::library(&doc)
                .photos
                .iter()
                .all(|photo| photo.favorite)
        );
        assert!(!app.selection_mode, "selection itself never persists");

        app.library_cards = vec![
            (first.id.clone(), Rect::new(0.0, 0.0, 44.0, 44.0), true),
            (second.id.clone(), Rect::new(48.0, 0.0, 92.0, 44.0), true),
        ];
        app.begin_selection();
        app.select_all_visible();
        let mut selected_ids = [first.id.clone(), second.id.clone()];
        selected_ids.sort_unstable();
        assert!(app.perform_library_semantic_action(
            "photos:selection:favorite",
            &AccessAction::Activate,
            &mut doc,
        ));
        let favorites = PhotosResources
            .resources(&doc)
            .pop()
            .expect("library root")
            .actions
            .into_iter()
            .find(|action| action.id == "set-favorites")
            .expect("favorite batch is published");
        let expected_favorite = favorites
            .build_args([
                (
                    "photo_ids".into(),
                    tilo_actions::ArgValue::Array(
                        selected_ids
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                ),
                ("favorite".into(), tilo_actions::ArgValue::Boolean(false)),
            ])
            .expect("published favorite contract accepts the selection result");
        assert_eq!(
            app.last_library_action.as_ref(),
            Some(&expected_favorite),
            "the AT batch control and resource action produce byte-identical envelopes"
        );
        assert!(
            PhotosDoc::library(&doc)
                .photos
                .iter()
                .all(|photo| !photo.favorite),
            "the same batch command also has an explicit unfavorite result"
        );

        app.begin_selection();
        app.select_all_visible();
        assert!(app.perform_library_semantic_action(
            "photos:selection:choose-album",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert!(app.selection_album_chooser);
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:trip",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(
            app.selection_destination_album.as_deref(),
            Some("photos.album:trip"),
            "the target album is an explicit, stable choice rather than the current view"
        );
        app.add_selected_to_destination_album(&mut doc);
        let album = PhotosResources
            .resource(&doc, "photos.album:trip")
            .expect("chosen album remains published");
        let membership = album
            .actions
            .into_iter()
            .find(|action| action.id == "set-membership")
            .expect("album membership is published");
        let expected_membership = membership
            .build_args([(
                "photo_ids".into(),
                tilo_actions::ArgValue::Array(
                    selected_ids
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            )])
            .expect("published membership contract accepts the selection result");
        assert_eq!(
            app.last_library_action.as_ref(),
            Some(&expected_membership),
            "the pointer batch control and resource action produce byte-identical envelopes"
        );
        assert_eq!(
            PhotosDoc::library(&doc).albums[0].photo_ids,
            vec![first.id, second.id],
            "album membership uses the same absolute document action agents invoke"
        );
    }

    #[test]
    fn destination_chooser_pages_every_album_and_refuses_a_stale_target() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let albums: Vec<_> = (1..=SELECTION_ALBUMS_PER_PAGE + 1)
            .map(|number| Album {
                id: format!("photos.album:destination-{number}"),
                name: format!("Destination {number}"),
                photo_ids: Vec::new(),
            })
            .collect();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo.clone()],
                albums: albums.clone(),
            },
        );
        app.library_albums = albums
            .iter()
            .map(|album| (album.id.clone(), album.name.clone(), 0))
            .collect();
        app.library_cards = vec![(photo.id.clone(), Rect::ZERO, false)];
        app.begin_selection();
        app.select_all_visible();
        assert!(app.perform_library_semantic_action(
            "photos:selection:choose-album",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination-next",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.selection_album_page, 1);
        assert!(app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:destination-7",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(
            app.selection_destination_album.as_deref(),
            Some("photos.album:destination-7")
        );
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![photo],
                albums: Vec::new(),
            },
        );
        assert!(!app.perform_library_semantic_action(
            "photos:selection:destination:photos.album:destination-7",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(
            app.notice.as_deref(),
            Some("That destination album is no longer available")
        );
    }

    #[test]
    fn unresolved_share_reference_returns_a_canonical_operation_failure() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let payload = tilo_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ShareReference {
                resource_id: "photos.photo:missing".into(),
            },
        )
        .expect("share operation encodes");

        let error = Widget::invoke_resource_operation(
            &mut app,
            tilo_actions::OperationKind::Effect,
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
    fn enlarged_library_keeps_density_navigation_without_metadata_chips() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let mut library = crate::model::PhotoLibrary::default();
        for index in 0..5 {
            let mut photo = LibraryPhoto::from_image(OpenImage::from_path(
                &format!("/photos/library-{index}.png"),
                20,
                10,
            ));
            photo.tags = vec!["golden hour".into()];
            library.photos.push(photo);
        }
        PhotosDoc::set_library(&mut doc, &library);
        app.show_library();
        app.large_text_toolbar = true;
        app.canvas_rect = Rect::new(0.0, 48.0, 390.0, 796.0);
        app.sync(&mut doc, &ROOT);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        text.set_text_scale(theme.text_scale);
        app.draw_library(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );

        assert!(app.metadata_filter_cards.is_empty());
        let new_album = app
            .album_editor_hits
            .iter()
            .find_map(|(hit, rect)| (*hit == AlbumEditorHit::New).then_some(*rect))
            .expect("200% library exposes New album");
        let header = compact_library_header_geometry(app.canvas_rect, true, false);
        assert!(
            new_album.y1 <= header.timeline_y,
            "New album must finish before the timeline begins: {new_album:?} / {header:?}"
        );
        let semantics = app.accessibility_tree();
        let children = &semantics.children[1].children;
        assert!(
            children.iter().any(|node| {
                node.action_target.as_deref() == Some("photos:timeline:density-in")
            })
        );
        assert!(!children.iter().any(|node| {
            node.action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("photos:library-page:"))
        }));

        let before = app.timeline_density;
        assert!(app.perform_library_semantic_action(
            "photos:timeline:density-in",
            &AccessAction::Focus,
            &mut doc,
        ));
        assert_eq!(
            app.timeline_density, before,
            "semantic focus must not navigate"
        );
        assert!(app.perform_library_semantic_action(
            "photos:timeline:density-in",
            &AccessAction::Activate,
            &mut doc,
        ));
        assert_eq!(app.timeline_density, before + 1);
        assert_eq!(PhotosDoc::library(&doc), library);
    }

    #[test]
    fn cancelling_a_portal_invocation_is_terminal_and_drops_late_results() {
        let mut app = PhotosApp::new();
        let doc = AutoCommit::new();
        let invocation = tilo_actions::InvocationId::new();
        let (tx, rx) = std::sync::mpsc::channel();
        app.picker = Some(rx);
        app.resource_invocation = Some(invocation);

        let update = Widget::poll_resource_operation(&mut app, invocation, true, &doc, &ROOT)
            .expect("cancellation must resolve the accepted invocation");
        assert!(matches!(
            update,
            tilo_actions::InvocationUpdate::Cancelled { invocation: id, .. } if id == invocation
        ));
        assert!(app.picker.is_none());
        assert!(app.resource_invocation.is_none());
        assert!(
            tx.send(ui::portal::PortalOutcome::Opened("/late.png".into()))
                .is_err(),
            "a late portal completion must have no receiver after cancellation"
        );

        let direct = tilo_actions::InvocationId::new();
        app.pending_resource_open = Some((direct, "/late-direct.png".into()));
        assert!(matches!(
            Widget::poll_resource_operation(&mut app, direct, true, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Cancelled { invocation, .. })
                if invocation == direct
        ));
        assert!(app.pending_resource_open.is_none());
    }

    #[test]
    fn concurrent_album_drafts_mint_distinct_stable_ids() {
        let doc = AutoCommit::new();
        let mut left = PhotosApp::new();
        let mut right = PhotosApp::new();
        left.begin_new_album(&doc);
        right.begin_new_album(&doc);
        let left_id = left.album_editor_id.expect("left draft id");
        let right_id = right.album_editor_id.expect("right draft id");
        assert!(left_id.starts_with("photos.album:"));
        assert!(right_id.starts_with("photos.album:"));
        assert_ne!(left_id, right_id);
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
    fn high_text_timeline_uses_readable_card_measures() {
        let width = 1_440.0;
        assert_eq!(timeline_column_count(width, 4, true), 2);
        let two_column_card_width = (width - f64::from(PAD * 2.0) - 8.0) / 2.0;
        assert!(
            two_column_card_width >= 600.0,
            "200% text needs a real reading measure, got {two_column_card_width}"
        );
    }

    #[test]
    fn regular_album_management_keeps_every_visible_control_at_44px() {
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
        app.sync(&mut doc, &ROOT);
        app.show_library();
        app.active_album = Some("photos.album:trip".into());
        app.canvas_rect = Rect::new(0.0, 0.0, 480.0, 900.0);

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let presence = PresenceCtx::empty();
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &presence,
        );
        assert_eq!(app.album_editor_hits.len(), 2);
        assert!(
            app.album_editor_hits
                .iter()
                .all(|(_, rect)| rect.height() >= 44.0)
        );

        app.begin_edit_album(&doc);
        app.draw_album_editor(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &presence,
        );

        assert!(
            app.album_editor_hits
                .iter()
                .all(|(_, rect)| rect.height() >= 44.0)
        );
        assert!(
            app.album_page_buttons
                .iter()
                .all(|(_, button)| button.get_rect().height() >= 44.0)
        );
        assert!(app.album_editor_hits.iter().all(|(_, rect)| {
            rect.x0 >= app.canvas_rect.x0
                && rect.x1 <= app.canvas_rect.x1
                && rect.y0 >= app.canvas_rect.y0
                && rect.y1 <= app.canvas_rect.y1
        }));
    }

    #[test]
    fn rejected_search_refresh_keeps_last_good_index_and_exports_diagnostic() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let first = LibraryPhoto::from_image(OpenImage::from_path("/photos/first.png", 20, 10));
        let first_id = first.id.clone();
        PhotosDoc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary {
                photos: vec![first],
                albums: Vec::new(),
            },
        );
        app.sync(&mut doc, &ROOT);
        assert!(app.search_provider.resource(&first_id).is_some());

        let mut library = PhotosDoc::library(&doc);
        let second = LibraryPhoto::from_image(OpenImage::from_path("/photos/second.png", 20, 10));
        let second_id = second.id.clone();
        library.photos.push(second);
        PhotosDoc::set_library(&mut doc, &library);
        app.force_search_refresh_error = true;
        app.sync(&mut doc, &ROOT);

        assert!(app.search_provider.resource(&first_id).is_some());
        assert!(app.search_provider.resource(&second_id).is_none());
        assert!(app.status.contains("kept its previous index"));
        let diagnostic = Widget::resource(&app, &doc, "photos.search-status")
            .expect("rejected refresh is an exact addressable resource");
        assert_eq!(diagnostic.kind, "photos.diagnostic");
        assert!(diagnostic.actions.iter().any(|action| action.id == "open"));
        assert!(
            Widget::resources(&app, &doc)
                .iter()
                .any(|resource| resource.id.0 == "photos.search-status")
        );
    }
}
