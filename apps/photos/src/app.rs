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

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use ui::time::Duration;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::AutoCommit;
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use tilo_content::ContentStore;

use crate::actions::PhotosEffect;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::ImageData;

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::animation::{Animated, Curve, Transition};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Button, ButtonFace, ButtonRole, DateIndexIntent, DateIndexMarker, DateIndexMarkerKind,
    DateIndexedScrollbar, EmptyState, EmptyStateContent, ImageViewer, ModalAction, ModalHit,
    ModalLayout, ModalSpec, ProgressBar, PropertyListFace, PropertyRow, ScrollMetrics, TextInput,
    TextInputKind, Widget,
};
use ui::widget::{ListRowFace, MediaCardFace, SegmentedFace, StatTileFace, ToggleGroup};
use winit::keyboard::{Key, NamedKey};

use crate::model::{Edits, LibraryPhoto, OpenImage, PhotoLibrary, Rect01};
use crate::pixels::{self, Rgba};
use crate::strings::{tr, tr_args, tr_count, tr_with};
use crate::timeline_projection::{TimelineGeometry, TimelinePhoto, TimelineProjection};
use ui::ArgValue;
use ui::tilo_resource::Id;

mod chrome;

/// A library card's action row: Details on the left, remove and favourite on
/// the right, inside the card's lower edge.
///
/// This is the *one* description of that row. `draw_library` paints these
/// rects, `handle_event` hit-tests them, `accessibility_tree` exports them and
/// `library_card_action_rects` hands them to tests.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardActions {
    details: Rect,
    remove: Rect,
    favorite: Rect,
}

/// The three hover actions of a timeline photograph, each a pointer target
/// inset `s2` from the photograph's lower edge.
fn card_action_controls(card: Rect, theme: &Theme) -> CardActions {
    let inset = f64::from(theme.tokens.spacing.s2);
    let side = f64::from(theme.tokens.metrics.min_pointer_target);
    let y0 = card.y1 - inset - side;
    let favorite = Rect::new(card.x1 - inset - side, y0, card.x1 - inset, y0 + side);
    let remove = Rect::new(favorite.x0 - side, y0, favorite.x0, y0 + side);
    let details = Rect::new(card.x0 + inset, y0, card.x0 + inset + side, y0 + side);
    CardActions {
        details,
        remove,
        favorite,
    }
}

/// Coarse, bundled coastlines for the private/offline Places overview. These
/// are intentionally only continental silhouettes: they add orientation but
/// cannot be mistaken for current navigation data or a precision basemap.
const OFFLINE_WORLD_LANDMASSES: &[&[(f64, f64)]] = &[
    &[
        (72.0, -168.0),
        (70.0, -140.0),
        (60.0, -125.0),
        (50.0, -124.0),
        (32.0, -117.0),
        (18.0, -104.0),
        (8.0, -83.0),
        (20.0, -74.0),
        (45.0, -61.0),
        (58.0, -72.0),
        (72.0, -95.0),
        (72.0, -168.0),
    ],
    &[
        (13.0, -81.0),
        (6.0, -77.0),
        (-8.0, -78.0),
        (-25.0, -70.0),
        (-55.0, -68.0),
        (-52.0, -42.0),
        (-20.0, -40.0),
        (2.0, -50.0),
        (13.0, -61.0),
        (13.0, -81.0),
    ],
    &[
        (36.0, -10.0),
        (51.0, -5.0),
        (61.0, 15.0),
        (70.0, 40.0),
        (58.0, 65.0),
        (72.0, 110.0),
        (62.0, 165.0),
        (45.0, 145.0),
        (20.0, 121.0),
        (7.0, 106.0),
        (22.0, 78.0),
        (10.0, 43.0),
        (31.0, 31.0),
        (36.0, -10.0),
    ],
    &[
        (35.0, -17.0),
        (31.0, 31.0),
        (12.0, 44.0),
        (-12.0, 43.0),
        (-35.0, 20.0),
        (-30.0, 10.0),
        (-5.0, 8.0),
        (15.0, -17.0),
        (35.0, -17.0),
    ],
    &[
        (-10.0, 113.0),
        (-18.0, 146.0),
        (-39.0, 147.0),
        (-44.0, 115.0),
        (-25.0, 112.0),
        (-10.0, 113.0),
    ],
    &[
        (-63.0, -180.0),
        (-70.0, -120.0),
        (-72.0, -60.0),
        (-68.0, 0.0),
        (-72.0, 60.0),
        (-69.0, 120.0),
        (-63.0, 180.0),
        (-63.0, -180.0),
    ],
];
/// Library previews are recognition aids, never an alternate full-resolution
/// image cache. At most four in flight on the shared preview pipeline's single
/// worker keeps a hostile library from occupying a core or retaining unbounded
/// decoded pixels.
const THUMBNAIL_SIZE: thumbnail::ThumbSize = thumbnail::ThumbSize::new(480, 480);
const THUMBNAIL_QUEUE_LIMIT: usize = 4;
const DIRECTORY_IMPORT_PER_FRAME: usize = 2;
/// A receipt must not retain an unbounded directory's diagnostics. The total
/// count remains visible; these are the concrete entries a person can inspect.
const DIRECTORY_FAILURE_DETAIL_LIMIT: usize = 20;
const ANALYSIS_QUEUE_LIMIT: usize = 2;
/// One bounded materialized library page. This is intentionally independent
/// of the number of cards that happen to fit in a particular window.
const LIBRARY_PAGE_SIZE: usize = 24;
/// People, places, and albums are projected one bounded page at a time. The
/// backing vectors retain the full collection so “Browse all” is real rather
/// than a prettier name for truncation.
const COLLECTION_PAGE_SIZE: usize = 4;

/// Target heights of a timeline row at each density step, in lines of body
/// text so they grow with the text scale: spacious (0) to compact (4).
const TIMELINE_ROW_LINES: [f64; 5] = [15.0, 11.5, 9.0, 7.0, 5.5];

/// The target height of a justified timeline row at `density` under
/// `theme`. Rows that fill the width are scaled from it; the photographs keep
/// their own aspect ratios.
fn timeline_row_height(theme: &Theme, density: u8) -> f64 {
    let line = f64::from(theme.text(ui::TextRole::Body).line_height_px());
    let lines = TIMELINE_ROW_LINES
        .get(usize::from(density))
        .copied()
        .unwrap_or(9.0);
    (line * lines).round()
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

/// The exact local batch admitted to the one analysis worker.  This is kept
/// in memory rather than in the CRDT (or the checkpoint) because it contains
/// no biometric result, and because a cursor is only durable *after* this
/// batch succeeds.
#[derive(Clone, Debug)]
struct PendingPeopleScan {
    start: usize,
    scanned: usize,
    total: usize,
}

impl PeopleScanProgress {
    /// Plan a bounded contiguous slice without moving the durable cursor.
    /// The caller commits `advance_after_success` only after the face worker
    /// has returned and its opaque local clusters were accepted.
    fn batch_range(&self, available: usize, limit: usize) -> Option<(usize, usize)> {
        (available > 0).then(|| {
            let start = self.next_offset % available;
            // Do not wrap a final partial batch: every locally available
            // photograph is covered once before the next pass begins.
            (start, limit.min(available.saturating_sub(start)))
        })
    }

    fn advance_after_success(&mut self, start: usize, scanned: usize, total: usize) {
        if total == 0 {
            return;
        }
        self.next_offset = start.saturating_add(scanned) % total;
        self.last_library_size = total;
        self.completed_batches = self.completed_batches.saturating_add(1);
        if self.next_offset == 0 {
            "complete"
        } else {
            "paused"
        }
        .clone_into(&mut self.status);
        self.last_error = None;
    }

    fn record_failure(&mut self, error: String) {
        // In particular, do not alter next_offset here: retry must send the
        // same slice, not skip photographs whose private analysis failed.
        "error".clone_into(&mut self.status);
        self.last_error = Some(error);
    }
    fn load() -> Self {
        Self::file()
            .and_then(|file| file.read())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let (Some(file), Ok(bytes)) = (Self::file(), serde_json::to_vec(self)) {
            let _ = file.write(&bytes);
        }
    }

    fn file() -> Option<crate::files::StateFile> {
        crate::files::StateFile::in_state("photos/people-scan.json")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageAnalysisComponent {
    Ocr,
    Vision,
}

#[expect(
    clippy::large_enum_variant,
    reason = "outcomes travel through a small bounded analysis channel and are consumed on the next frame; boxing would add an allocation per job for no meaningful saving"
)]
enum AnalysisOutcome {
    Image {
        component: ImageAnalysisComponent,
        result: Result<crate::model::ImageAnalysis, String>,
    },
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

fn mark_analysis_shared(analysis: &mut crate::model::ImageAnalysis) {
    "shared".clone_into(&mut analysis.provenance.scope);
    if let Some(provenance) = &mut analysis.ocr_provenance {
        "shared".clone_into(&mut provenance.scope);
    }
    if let Some(provenance) = &mut analysis.vision_provenance {
        "shared".clone_into(&mut provenance.scope);
    }
}

fn analysis_worker(
    rx: &Receiver<AnalysisJob>,
    tx: &SyncSender<AnalysisResult>,
    content: &ContentStore,
) {
    while let Ok(job) = rx.recv() {
        if tx
            .send(AnalysisResult {
                invocation: job.invocation,
                photo_id: job.photo_id,
                content: job.content,
                result: match job.kind {
                    AnalysisJobKind::Ocr { language } => AnalysisOutcome::Image {
                        component: ImageAnalysisComponent::Ocr,
                        result: crate::analysis::recognize_text(&job.image, &language, content),
                    },
                    AnalysisJobKind::Vision { capability } => AnalysisOutcome::Image {
                        component: ImageAnalysisComponent::Vision,
                        result: crate::analysis::analyze_image(&job.image, &capability, content),
                    },
                    AnalysisJobKind::People { capability, images } => AnalysisOutcome::People(
                        crate::analysis::detect_people(images, &capability, content),
                    ),
                },
            })
            .is_err()
        {
            return;
        }
    }
}

/// Wake the idle host whenever a preview settles, success or failure, so a
/// finished thumbnail paints without waiting for user input.
fn with_thumbnail_wake(service: thumbnail::ThumbnailService) -> thumbnail::ThumbnailService {
    let wake = ui::wake::WakeHandle::new();
    service.set_wake(move || wake.wake());
    service
}

/// The shared-pipeline request for one library photograph. A content
/// reference is the preview's content address and persists in the preview
/// cache; a legacy path-only photograph has no stable content identity, so its
/// preview is generated for this session only.
fn thumbnail_request(image: &OpenImage, content: &ContentStore) -> thumbnail::ThumbRequest {
    let persistent = image
        .content
        .as_deref()
        .and_then(|address| thumbnail::ThumbKey::of_address(address, THUMBNAIL_SIZE));
    let key = persistent.unwrap_or_else(|| {
        thumbnail::ThumbKey::of_bytes(decode_key(image).as_bytes(), THUMBNAIL_SIZE)
    });
    let (image, content) = (image.clone(), content.clone());
    let request = thumbnail::ThumbRequest::load(key, thumbnail::FileCategory::Image, move || {
        crate::model::bytes_of(&image, &content)
    });
    if persistent.is_some() {
        request
    } else {
        request.transient()
    }
}

/// One header command. Commands post their intent here rather than
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
    Select,
    NewAlbum,
    EditAlbum,
    Smaller,
    Larger,
    PlacesList,
    PlacesZoomIn,
    PlacesZoomOut,
    Selection(SelectionHit),
    PeopleHidden,
    Search,
    More,
    PreviousPage,
    NextPage,
}

/// Controls inside the in-grid album editor. They are intentionally a small
/// local presentation state machine; every durable outcome is still one
/// album insert, or its name and photographs edited together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlbumEditorHit {
    New,
    Save,
    Cancel,
    Delete,
    ConfirmDelete,
    Edit,
    PreviousPage,
    NextPage,
}

/// Controls in the photo-details sheet.  The fields themselves are shared
/// `TextInput`s; these are only the surrounding sheet actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataEditorHit {
    Save,
    Cancel,
    Organize,
    TimeAndPlace,
    ClearFolder,
    ClearPeople,
    ClearTags,
    ClearCapturedAt,
    ClearLocation,
    /// Set the draft location to the centre of the offline reference map.
    /// Pointer activation uses the exact point under the pointer instead.
    PlaceOnMap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MetadataEditorSection {
    #[default]
    Organize,
    TimeAndPlace,
}

/// The local selection shelf. Selection is intentionally not a CRDT value:
/// collaborators should see the durable favourite/album result, never a
/// private cursor-like set that changes while somebody is deciding.
const SELECTION_ALBUMS_PER_PAGE: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionHit {
    PreviousDestinationPage,
    NextDestinationPage,
    Start,
    SelectAll,
    Clear,
    Favorite,
    EditDetails,
    ChooseAlbum,
    CancelDestinationChooser,
    CloseDestinationChooser,
    ApplyDestination,
    AddToAlbum,
    Done,
}

/// The library's three primary browse destinations. This is presentation-only
/// and deliberately separated from durable albums/metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryDestination {
    Timeline,
    Albums,
    People,
    Places,
    Analysis,
}

#[derive(Clone)]
struct PlaceItem {
    photo: LibraryPhoto,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlacesHit {
    /// A canonical place key, never a representative photo id.  A marker is
    /// a collection affordance and must not silently choose one photo from it.
    Place(String),
    /// A photo row inside an already-open place collection.
    Photo(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CollectionPageHit {
    AlbumsPrevious,
    AlbumsNext,
    PeoplePrevious,
    PeopleNext,
    PlacesPrevious,
    PlacesNext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryStatusHit {
    Stop,
    Details,
    Dismiss,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeopleSelection {
    Approved(String),
    Suggestion(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PeopleIntent {
    Page(bool),
    Back,
    ToggleRecognition,
    Scan,
    CancelScan,
    OpenApproved(String),
    OpenSuggestion(String),
    BrowseApproved,
    SaveName,
    BeginMerge,
    MergeApprovedInto(String),
    ApproveSuggestion,
    RejectSuggestion,
    HideSuggestion,
    RestoreSuggestion,
    MergeSuggestionWith(String),
    ToggleHidden,
    DeleteLocalData,
    ConfirmDeleteLocalData,
}

struct PeopleControl {
    intent: PeopleIntent,
    label: String,
    bounds: Rect,
    button: Button,
    /// The control's width as last measured in paint; the next layout
    /// places it with this.
    width: f64,
}

impl PeopleControl {
    /// A person or a suggestion is opened by its card, not by a button.
    fn is_card(&self) -> bool {
        matches!(
            self.intent,
            PeopleIntent::OpenApproved(_) | PeopleIntent::OpenSuggestion(_)
        )
    }
}

/// Where the People surface's text and photographs land, computed with its
/// controls so paint and hit-testing agree.
#[derive(Clone, Debug, Default)]
struct PeopleLayout {
    recognition_heading: Option<Point>,
    recognition_text: Rect,
    people_heading: Option<Point>,
    people_empty: Option<Rect>,
    suggestions_heading: Option<Point>,
    suggestions_empty: Option<Rect>,
    detail_photos: Vec<Rect>,
    detail_title: Option<Point>,
    detail_text: Rect,
    name_label: Option<Point>,
    /// Where the cards scroll, and how tall they are together.
    lanes: Rect,
    lanes_content: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryAnalysisHit {
    Start,
    Pause,
    Resume,
    RetryFailures,
}

/// A compact materialised card.  The full library remains canonical in the
/// document; this carries just enough metadata to make the visible shelf
/// scannable without another document walk while painting.
#[derive(Clone)]
struct LibraryItem {
    id: String,
    name: String,
    favorite: bool,
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

/// Authored content observed by local transient work, excluding operational metadata.
#[derive(Clone, PartialEq)]
struct LiveContent {
    library: crate::model::PhotoLibrary,
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

/// The compact, explicitly shared context shown beside an open photograph.
///
/// This is a projection of the library record, not a second metadata store:
/// it gives the viewer useful orientation without turning a filename or a
/// device-local EXIF read into collaboration state.  The persistent editor
/// remains the library's canonical metadata action surface.
#[derive(Clone, Debug)]
struct ViewerDetails {
    name: String,
    description: Option<String>,
    captured_at: Option<String>,
    place: Option<String>,
    people: Vec<String>,
    tags: Vec<String>,
    folder: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "library mode and the three independent transient album-editor flags have distinct event and reconciliation lifecycles"
)]
pub struct PhotosApp {
    /// The library's door: every edit this surface makes is one of the
    /// library's actions, applied through it. `None` until the framework
    /// attaches the surface; nothing is edited before then.
    dispatch: Option<ui::framework::Dispatcher<crate::model::PhotoLibrary>>,
    /// The photographs and albums a search hit or an agent's `open` asked
    /// to be shown, shown on the next sync.
    opens: Rc<RefCell<Vec<Id>>>,
    /// The library's published actions and this surface's effects, which
    /// its retained controls are bound to once the framework attaches it.
    actions: Option<ui::framework::Actions<crate::model::PhotoLibrary>>,
    /// Where imported originals are kept and photographs are read from. The
    /// document names content by hash; this handle resolves it.
    app_name: String,
    content: ContentStore,
    /// Test-only evidence that each visible batch route creates the exact
    /// envelope the corresponding published action descriptor creates.
    #[cfg(test)]
    last_library_action: Option<ui::tilo_resource::Invoke<PhotoLibrary>>,
    /// Decoded original, before any edit. `None` until a file opens.
    original: Option<(String, Rgba)>,
    original_identity: Option<String>,
    observed_content: Option<LiveContent>,
    import_content: Option<LiveContent>,
    picker_stale: bool,
    rendered: Option<Rendered>,
    viewer: Option<ImageViewer>,
    /// Bounded presentation projection for the open-view information card.
    /// It is refreshed from the document and never written back from paint.
    viewer_details: Option<ViewerDetails>,
    /// The sidebar, the header row and the status bar.
    chrome: chrome::Chrome,
    /// The theme of the last layout, for geometry read outside a paint
    /// (hit-testing and accessibility bounds).
    layout_theme: Theme,
    pending: Rc<Cell<Option<Hit>>>,
    /// Where the last export went, or why it failed. Shown once in the status
    /// bubble so an action is never silent.
    notice: Option<String>,
    status_bubble: ui::StatusBubble,
    /// The empty state's one action, where it was painted.
    empty_state: Option<EmptyState>,
    empty_state_key: Option<(String, String, bool)>,
    rect: Rect,
    canvas_rect: Rect,
    status_rect: Rect,
    /// Portal channel while a file chooser is open.
    picker: Option<ui::portal::PortalChannel>,
    /// The chooser that delivers chosen files' contents, where files have no
    /// path (a browser page).
    contents_picker: Option<ui::portal::ContentsChannel>,
    directory_picker: Option<ui::portal::PortalChannel>,
    directory_picker_retain_exif_location: bool,
    directory_import: Option<DirectoryImport>,
    /// Last completed local job remains visible until the next one begins.
    /// Paths stay device-local and are reduced to file names in presentation.
    directory_import_receipt: Option<DirectoryImportReceipt>,
    directory_import_details_open: bool,
    /// The issues list in the details sheet: every kept issue, scrolled
    /// inside the sheet's body.
    directory_import_details_scroll: ui::widget::ScrollRegion,
    directory_import_details_layout: Option<ModalLayout>,
    directory_import_pending: Rc<Cell<Option<DirectoryStatusHit>>>,
    directory_import_buttons: Vec<(DirectoryStatusHit, Button)>,
    directory_import_progress: ProgressBar,
    resource_invocation: Option<tilo_actions::InvocationId>,
    /// The answers owed to the callers of effects whose work continues,
    /// by the invocation that work reports under.
    replies: HashMap<tilo_actions::InvocationId, ui::framework::EffectReply>,
    /// Every report `settle` gave, for a test of the surface's own methods,
    /// which has no caller to receive them.
    #[cfg(test)]
    settled: Vec<tilo_actions::InvocationUpdate>,
    pending_resource_open: Option<(tilo_actions::InvocationId, String)>,
    /// The local analysis worker's queue, started with the first job: a
    /// window that analyses nothing (a past version History shows) starts
    /// no worker.
    analysis_tx: Option<SyncSender<AnalysisJob>>,
    analysis_result_tx: SyncSender<AnalysisResult>,
    analysis_rx: Receiver<AnalysisResult>,
    active_analysis: HashSet<tilo_actions::InvocationId>,
    cancelled_analysis: HashSet<tilo_actions::InvocationId>,
    active_people_scans: HashSet<tilo_actions::InvocationId>,
    pending_people_scans: HashMap<tilo_actions::InvocationId, PendingPeopleScan>,
    library_analysis: crate::library_analysis::LibraryAnalysisState,
    library_analysis_capability: Option<tilo_vision::LocalVisionCapability>,
    library_analysis_pending: Rc<Cell<Option<LibraryAnalysisHit>>>,
    library_analysis_buttons: Vec<(LibraryAnalysisHit, Button)>,
    library_analysis_progress: ProgressBar,
    library_analysis_failure_hits: Vec<(String, Rect)>,
    /// (shared analyzed, locally available unprocessed, awaiting local bytes)
    /// over the complete library, never the current visual filter.
    library_analysis_coverage: (usize, usize, usize),
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
    people_selection: Option<PeopleSelection>,
    /// The person review sheet enters with the shared motion primitive.  The
    /// renderer snaps it open when reduced motion is requested.
    people_detail_transition: Transition,
    people_merge_mode: bool,
    people_show_hidden: bool,
    /// How far the People cards are scrolled under the recognition section.
    people_scroll: f64,
    people_approved: Vec<crate::people::SharedPerson>,
    people_delete_confirm: bool,
    people_name_input: TextInput,
    people_name_bounds: Rect,
    people_layout: PeopleLayout,
    people_controls: Vec<PeopleControl>,
    people_control_signature: String,
    people_pending: Rc<RefCell<Option<PeopleIntent>>>,
    canvas_state: CanvasState,
    library_index: crate::search::LibraryIndex,
    /// Heads that produced the cached immutable/Arc-backed search catalogue.
    /// An unchanged `sync` must not rebuild a full-library index.
    search_revision: Option<u64>,
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
    /// Every photograph in the shared library, whatever the view filters.
    library_photo_count: usize,
    /// A screenshot scenario to seed into the document on the next sync, and
    /// where its generated photographs go.
    /// Set when the viewer is replaced. The layout tree points at it, so the
    /// frame that built it must be rebuilt before anything measures through
    /// the old pointer.
    structure_dirty: bool,
    library_destination: LibraryDestination,
    places_list_mode: bool,
    places_viewport: crate::places::MapViewport,
    /// Frame every place on the next paint of the map.
    places_fit_pending: bool,
    /// The exact canonical place collection currently opened from a resource,
    /// map marker, keyboard, or accessibility action. Presentation-only.
    places_open_key: Option<String>,
    places_items: Vec<PlaceItem>,
    places_page: usize,
    places_hits: Vec<(PlacesHit, Rect)>,
    collection_page_hits: Vec<(CollectionPageHit, Rect, bool)>,
    collection_page_buttons: Vec<(CollectionPageHit, Button)>,
    collection_page_pending: Rc<Cell<Option<CollectionPageHit>>>,
    places_map_bounds: Rect,
    places_drag_origin: Option<Point>,
    /// A short local response to a map pan or zoom. It deliberately animates
    /// only presentation geometry—camera position itself stays immediate so
    /// keyboard, pointer, agents, and screen readers all observe the same
    /// completed operation at once.
    places_camera_feedback: Option<Animated<f64>>,
    places_motion_reduced: bool,
    /// Count for the always-addressable Favorites virtual collection. This is
    /// a scalar projection of the shared library, so paint never walks the
    /// document and the chip stays bounded even for a very large collection.
    library_favorite_count: usize,
    library_offset: usize,
    library_albums: Vec<(String, String, usize)>,
    /// At most four stable member ids per album for the bounded cover mosaic.
    library_album_covers: HashMap<String, Vec<String>>,
    /// How far the album grid is scrolled, and the region it scrolls in.
    albums_scroll: f64,
    albums_page: usize,
    albums_viewport: Rect,
    selection_album_page: usize,
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
    people_page: usize,
    /// One bounded representative image per approved person, selected from
    /// the shared metadata projection and rendered from the existing cache.
    library_person_covers: HashMap<String, String>,
    library_tags: Vec<(String, usize)>,
    metadata_filter_cards: Vec<MetadataFilterCard>,
    /// Visible paging affordances for a one-card enlarged-text shelf. These
    /// are local navigation only; their stable semantic targets let keyboard
    /// and assistive-technology users reach the same bounded slice.
    library_page_hits: Vec<(bool, bool, Rect)>,
    /// Number of visual density steps, from spacious (0) to compact (4).
    /// This remains local UI state: collaborators should never change a
    /// person's preferred thumbnail size.
    timeline_density: u8,
    /// Exact variable-height chronology coordinate.
    timeline_scroll_y: f64,
    timeline_projection: TimelineProjection,
    timeline_projection_revision: Option<u64>,
    timeline_projection_filter: String,
    timeline_viewport_height: f64,
    /// Shared chronological navigation face.  It owns no library data: the
    /// current row offset/density remain private presentation state, while
    /// date boundaries are a bounded projection of canonical photo metadata.
    timeline_index: DateIndexedScrollbar,
    timeline_index_markers: Vec<DateIndexMarker>,
    timeline_index_metrics: ScrollMetrics,
    library_details_cards: Vec<(String, Rect)>,
    album_cards: Vec<(Option<String>, Rect)>,
    album_edit_cards: Vec<(String, Rect)>,
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
    /// An album sheet command invoked through its effect, for the next sync.
    album_editor_pending: Cell<Option<AlbumEditorHit>>,
    metadata_editor: bool,
    metadata_editor_ids: Vec<String>,
    metadata_folder_input: TextInput,
    metadata_people_input: TextInput,
    metadata_tags_input: TextInput,
    metadata_captured_at_input: TextInput,
    metadata_place_input: TextInput,
    metadata_latitude_input: TextInput,
    metadata_longitude_input: TextInput,
    metadata_location_map: crate::places::MapViewport,
    metadata_location_map_bounds: Rect,
    /// A draft-only provenance choice. It becomes durable only on Save, and
    /// lets collaborators distinguish deliberate map placement from typing.
    metadata_location_from_map: bool,
    /// Whether each field started with multiple values in a multi-photo edit.
    /// An untouched empty mixed field means Keep, never Clear.
    metadata_mixed: [bool; 5],
    /// A visible Clear command resolves the otherwise ambiguous empty mixed
    /// field without magic sentinel text.
    metadata_force_clear: [bool; 5],
    metadata_editor_section: MetadataEditorSection,
    /// Tab order within the active section plus Cancel and Save.
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
    /// The metadata sheet is laid out over the media canvas, but its command
    /// controls are still real toolkit buttons. Keeping them retained gives
    /// hover/press/focus treatment, native action publication and keyboard
    /// activation to the same controls a pointer user sees.
    metadata_editor_buttons: Vec<(MetadataEditorHit, Button)>,
    metadata_editor_pending: Rc<Cell<Option<MetadataEditorHit>>>,
    /// At most two viewportfuls of decoded thumbnails.  Originals are never
    /// scanned at draw time, and a large library cannot turn a frame into a
    /// disk-I/O loop.
    library_thumbnails: HashMap<String, ImageData>,
    thumbnail_resource_keys: HashMap<String, String>,
    /// The visible entries whose bytes were unavailable or invalid in the
    /// current document generation. Keeping this negative cache avoids
    /// decoding the same corrupt peer payload every frame; it is cleared when
    /// shared metadata changes so a fresh content reference gets a retry.
    thumbnail_unavailable: HashSet<String>,
    /// A bad peer-provided image must not be decoded once per frame. Content
    /// hashes are immutable, and a user reopening a changed legacy path makes
    /// a new attempt through `open_path`, so retaining this failure is safe.
    original_decode_failed: Option<String>,
    /// The shared preview pipeline; its one worker serializes disk and
    /// content-store access. Results are stamped with a visible-library
    /// generation before they may enter the UI cache, so an old job can never
    /// paint a card that now names another photograph.
    thumbnail_service: thumbnail::ThumbnailService,
    /// The pipeline key of every queued preview, by thumbnail id.
    thumbnail_keys: HashMap<String, thumbnail::ThumbKey>,
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
    /// The chooser lists every album, scrolled inside the sheet's body.
    selection_album_scroll: ui::widget::ScrollRegion,
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
    /// The resource state the framework presented last.
    presented: Rc<crate::model::PhotoLibrary>,
    /// Counts presentations, so derived indexes rebuild only when the
    /// presented state changed.
    presented_revision: u64,
    /// The photograph this window shows: its own view state, never shared.
    /// Opening one here opens nothing for a collaborator.
    open_photo: Option<String>,
    /// What the window was launched on: a folder to import, or the
    /// photograph the launch imported, to open.
    launched: Option<std::path::PathBuf>,
    /// The screenshot scenario whose view state the next sync stages.
    staged: Option<String>,
}

struct DirectoryImport {
    receiver: Receiver<DirectoryImportEvent>,
    cancel: Arc<AtomicBool>,
    invocation: Option<tilo_actions::InvocationId>,
    mode: DirectoryImportMode,
    discovered: usize,
    imported: usize,
    refreshed: usize,
    unchanged: usize,
    skipped: usize,
    sequence: u64,
    pending: VecDeque<crate::import::IndexedPhoto>,
    failures: Vec<crate::import::ImportFailure>,
    worker_done: bool,
    cancelled: bool,
    limit_reached: bool,
    produced_resources: Vec<String>,
    /// Keeps deterministic offscreen progress fixtures alive. Real jobs own
    /// their sender on the worker and always leave this empty.
    _shot_sender: Option<std::sync::mpsc::Sender<DirectoryImportEvent>>,
}

enum DirectoryImportEvent {
    Progress(crate::import::ImportProgress),
    Finished(Result<crate::import::ImportReport, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryImportMode {
    Import,
    Reindex,
}

#[derive(Clone, Debug)]
struct DirectoryImportReceipt {
    mode: DirectoryImportMode,
    discovered: usize,
    imported: usize,
    refreshed: usize,
    unchanged: usize,
    failures: Vec<crate::import::ImportFailure>,
    failure_count: usize,
    cancelled: bool,
    limit_reached: bool,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    /// What the notice says when an analysis run finishes.
    fn analysis_finished_notice(&self) -> String {
        tr_args(
            "photos-analysis-finished-notice",
            &[
                (
                    "analyzed",
                    ArgValue::Number(self.library_analysis.succeeded as f64),
                ),
                (
                    "failed",
                    ArgValue::Number(self.library_analysis.failed as f64),
                ),
            ],
        )
    }

    fn discover_library_analysis_capability() -> Result<tilo_vision::LocalVisionCapability, String>
    {
        tilo_vision::discover_local_capability(&agent_provider_config::ProviderConfig::load())
            .ok_or_else(|| tr("photos-analysis-error-no-model"))
    }

    fn begin_library_analysis(
        &mut self,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
        retry_failures: bool,
    ) -> Result<(), String> {
        if self.library_analysis.phase == crate::library_analysis::LibraryAnalysisPhase::Running {
            return Err(tr("photos-analysis-error-running"));
        }
        let capability = Self::discover_library_analysis_capability()?;
        if retry_failures {
            if !self.library_analysis.begin_retry(
                invocation,
                capability.model_label.clone(),
                capability.revision.clone(),
            ) {
                return Err(tr("photos-analysis-error-no-failures"));
            }
        } else {
            let mut unavailable = 0;
            let targets = library
                .photos
                .values()
                .filter(|photo| {
                    !photo.analysis().is_some_and(|analysis| {
                        analysis.has_vision_component(
                            photo.image.content.as_deref(),
                            &capability.revision,
                        )
                    })
                })
                .filter_map(|photo| {
                    if photo.image.content.is_some()
                        && crate::model::bytes_of(&photo.image, &self.content).is_some()
                    {
                        Some(photo.id.clone())
                    } else {
                        unavailable += 1;
                        None
                    }
                })
                .collect::<Vec<_>>();
            self.library_analysis.begin(
                invocation,
                targets,
                unavailable,
                capability.model_label.clone(),
                capability.revision.clone(),
            );
        }
        self.library_analysis_capability = Some(capability);
        self.active_analysis.insert(invocation);
        self.schedule_next_library_analysis(library)?;
        Ok(())
    }

    fn resume_library_analysis(
        &mut self,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        let capability = Self::discover_library_analysis_capability()?;
        if self.library_analysis.model_revision.as_deref() != Some(capability.revision.as_str()) {
            return Err(tr("photos-analysis-error-model-changed"));
        }
        if !self.library_analysis.resume(invocation) {
            return Err(tr("photos-analysis-error-not-paused"));
        }
        self.library_analysis_capability = Some(capability);
        self.active_analysis.insert(invocation);
        self.schedule_next_library_analysis(library)?;
        Ok(())
    }

    fn pause_library_analysis(&mut self) -> bool {
        let invocation = self.library_analysis.invocation;
        let paused = self.library_analysis.pause();
        if paused {
            if let Some(invocation) = invocation {
                self.active_analysis.remove(&invocation);
            }
            self.notice = Some(tr("photos-analysis-pausing"));
        }
        paused
    }

    fn activate_library_analysis(&mut self, hit: LibraryAnalysisHit) -> bool {
        let invocation = tilo_actions::InvocationId::new();
        let library = self.presented();
        let result = match hit {
            LibraryAnalysisHit::Start => self.begin_library_analysis(invocation, &library, false),
            LibraryAnalysisHit::Pause => self
                .pause_library_analysis()
                .then_some(())
                .ok_or_else(|| "Photo analysis is not running".to_owned()),
            LibraryAnalysisHit::Resume => self.resume_library_analysis(invocation, &library),
            LibraryAnalysisHit::RetryFailures => {
                self.begin_library_analysis(invocation, &library, true)
            }
        };
        if let Err(error) = result {
            self.notice = Some(error);
            return false;
        }
        true
    }

    fn schedule_next_library_analysis(
        &mut self,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        loop {
            let Some(photo_id) = self
                .library_analysis
                .take_next(|id| library.photo(id).cloned().map(|photo| photo.image.name))
            else {
                if self.library_analysis.finish_if_empty()
                    && let Some(invocation) = self.library_analysis.invocation
                {
                    self.active_analysis.remove(&invocation);
                    self.settle(tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!({
                                "analyzed": self.library_analysis.succeeded,
                                "failed": self.library_analysis.failed,
                                "unavailable": self.library_analysis.unavailable,
                            })),
                            resources: vec!["photos.library-analysis".to_owned()],
                        },
                    });
                    self.notice = Some(self.analysis_finished_notice());
                }
                return Ok(());
            };
            let Some(photo) = library.photo(&photo_id).cloned() else {
                let _ = self.library_analysis.record_failure(
                    crate::library_analysis::LibraryAnalysisFailure {
                        photo_id: photo_id.clone(),
                        name: photo_id,
                        message: tr("photos-analysis-error-removed"),
                    },
                );
                continue;
            };
            let capability = self
                .library_analysis_capability
                .clone()
                .ok_or_else(|| tr("photos-analysis-error-model-gone"))?;
            let invocation = self
                .library_analysis
                .invocation
                .ok_or_else(|| tr("photos-analysis-error-lost"))?;
            let sender = self
                .analysis_sender()
                .ok_or_else(|| "local image analysis is unavailable on this device".to_owned())?;
            sender
                .try_send(AnalysisJob {
                    invocation,
                    photo_id,
                    content: photo.image.content.clone(),
                    image: photo.image,
                    kind: AnalysisJobKind::Vision { capability },
                })
                .map_err(|_| tr("photos-analysis-error-busy"))?;
            return Ok(());
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each analysis outcome variant is reconciled against the presented library and library-analysis progress in one exhaustive match"
    )]
    fn drain_library_analysis_result(&mut self, completed: AnalysisResult) {
        let photo_name = self
            .library_analysis
            .current_name
            .clone()
            .unwrap_or_else(|| completed.photo_id.clone());
        let stale = self
            .presented()
            .photo(&completed.photo_id)
            .cloned()
            .as_ref()
            .is_none_or(|photo| photo.image.content != completed.content);
        let item_result = match completed.result {
            AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Vision,
                result: Ok(mut analysis),
            } if !stale => {
                mark_analysis_shared(&mut analysis);
                self.submit(
                    PhotoLibrary::MERGE_IMAGE_ANALYSIS
                        .with((Id::from_raw(completed.photo_id.clone()), analysis)),
                )
            }
            AnalysisOutcome::Image {
                component: ImageAnalysisComponent::Ocr,
                ..
            }
            | AnalysisOutcome::People(_) => Err(tr("photos-analysis-error-wrong-result")),
            AnalysisOutcome::Image { result: Ok(_), .. } => {
                Err(tr("photos-analysis-error-changed"))
            }
            AnalysisOutcome::Image {
                result: Err(error), ..
            } => Err(error),
        };
        match item_result {
            Ok(()) => {
                let _ = self.library_analysis.record_success(&completed.photo_id);
            }
            Err(message) => {
                let _ = self.library_analysis.record_failure(
                    crate::library_analysis::LibraryAnalysisFailure {
                        photo_id: completed.photo_id,
                        name: photo_name,
                        message,
                    },
                );
            }
        }
        if let Some(invocation) = self.library_analysis.invocation {
            if self.library_analysis.phase
                == crate::library_analysis::LibraryAnalysisPhase::Complete
            {
                self.active_analysis.remove(&invocation);
                self.settle(tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "analyzed": self.library_analysis.succeeded,
                            "failed": self.library_analysis.failed,
                            "unavailable": self.library_analysis.unavailable,
                        })),
                        resources: vec!["photos.library-analysis".to_owned()],
                    },
                });
                self.notice = Some(self.analysis_finished_notice());
            } else {
                self.settle(tilo_actions::InvocationUpdate::Progress {
                    invocation,
                    sequence: u64::try_from(self.library_analysis.completed).unwrap_or(u64::MAX),
                    phase: if self.library_analysis.phase
                        == crate::library_analysis::LibraryAnalysisPhase::Paused
                    {
                        "paused".to_owned()
                    } else {
                        "analyzing".to_owned()
                    },
                    completed: Some(
                        u64::try_from(self.library_analysis.completed).unwrap_or(u64::MAX),
                    ),
                    total: Some(u64::try_from(self.library_analysis.total).unwrap_or(u64::MAX)),
                    message: Some(tr_args(
                        "photos-analysis-tally",
                        &[
                            (
                                "analyzed",
                                ArgValue::Number(self.library_analysis.succeeded as f64),
                            ),
                            (
                                "failed",
                                ArgValue::Number(self.library_analysis.failed as f64),
                            ),
                        ],
                    )),
                });
            }
        }
        if self.library_analysis.phase == crate::library_analysis::LibraryAnalysisPhase::Running
            && let Err(error) = self.schedule_next_library_analysis(&self.presented())
        {
            self.library_analysis.pause();
            self.notice = Some(tr_with("photos-analysis-paused-because", "reason", &error));
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one drain loop applies every completed analysis job kind with the same stale/cancelled checks"
    )]
    fn drain_analysis_results(&mut self) {
        while let Ok(completed) = self.analysis_rx.try_recv() {
            if self.library_analysis.invocation == Some(completed.invocation) {
                self.drain_library_analysis_result(completed);
                continue;
            }
            self.active_analysis.remove(&completed.invocation);
            self.active_people_scans.remove(&completed.invocation);
            let pending_people_scan = self.pending_people_scans.remove(&completed.invocation);
            if self.cancelled_analysis.remove(&completed.invocation) {
                continue;
            }
            let stale = matches!(&completed.result, AnalysisOutcome::Image { .. })
                && self
                    .presented()
                    .photo(&completed.photo_id)
                    .cloned()
                    .as_ref()
                    .is_none_or(|photo| photo.image.content != completed.content);
            let update = if stale {
                tilo_actions::InvocationUpdate::Failed {
                    invocation: completed.invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.analysis_stale".into(),
                        message: tr("photos-analysis-error-changed-retry"),
                        details: None,
                        retryable: true,
                    },
                }
            } else {
                match completed.result {
                    AnalysisOutcome::Image {
                        component: _,
                        result: Ok(mut analysis),
                    } => {
                        mark_analysis_shared(&mut analysis);
                        match self.submit(
                            PhotoLibrary::MERGE_IMAGE_ANALYSIS
                                .with((Id::from_raw(completed.photo_id), analysis)),
                        ) {
                            Ok(()) => {
                                self.notice = Some(tr("photos-analysis-shared"));
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                            Err(error) => analysis_failure(completed.invocation, error),
                        }
                    }
                    AnalysisOutcome::Image {
                        result: Err(error), ..
                    } => analysis_failure(completed.invocation, error),
                    AnalysisOutcome::People(Err(error)) => {
                        self.people_scan_progress.record_failure(error.clone());
                        self.people_scan_progress.save();
                        analysis_failure(completed.invocation, error)
                    }
                    AnalysisOutcome::People(Ok(batch)) => {
                        let added = self.people_controller.ingest_verified_face_clusters(batch);
                        if let Some(pending) = pending_people_scan {
                            self.people_scan_progress.advance_after_success(
                                pending.start,
                                pending.scanned,
                                pending.total,
                            );
                            self.people_scan_progress.save();
                            self.notice = Some(if added == 0 {
                                tr("photos-people-scan-none")
                            } else {
                                tr_count("photos-people-scan-found", added)
                            });
                            if self.people_scan_progress.next_offset != 0 {
                                if let Err(error) = self.schedule_people_scan_batch(
                                    completed.invocation,
                                    &self.presented(),
                                ) {
                                    self.people_scan_progress.record_failure(error.clone());
                                    self.people_scan_progress.save();
                                    analysis_failure(completed.invocation, error)
                                } else {
                                    tilo_actions::InvocationUpdate::Progress {
                                        invocation: completed.invocation,
                                        sequence: self.people_scan_progress.completed_batches,
                                        phase: "scanning".to_owned(),
                                        completed: Some(
                                            u64::try_from(self.people_scan_progress.next_offset)
                                                .unwrap_or(u64::MAX),
                                        ),
                                        total: Some(
                                            u64::try_from(
                                                self.people_scan_progress.last_library_size,
                                            )
                                            .unwrap_or(u64::MAX),
                                        ),
                                        message: Some(tr("photos-people-scan-next")),
                                    }
                                }
                            } else {
                                tilo_actions::InvocationUpdate::Succeeded {
                                    invocation: completed.invocation,
                                    output: tilo_actions::ActionOutput::default(),
                                }
                            }
                        } else {
                            self.people_scan_progress
                                .record_failure(tr("photos-people-scan-lost"));
                            self.people_scan_progress.save();
                            analysis_failure(completed.invocation, tr("photos-people-scan-lost"))
                        }
                    }
                }
            };
            self.settle(update);
        }
    }

    /// An app whose content store lives in memory with it: tests and offscreen
    /// shots. The runtime uses [`Self::new_persistent`].
    pub fn new() -> Self {
        Self::with_content(ContentStore::memory())
    }

    /// An app keeping imported originals in, and reading photographs from,
    /// `content`. The handle is shared with the app's background workers.
    #[allow(
        clippy::too_many_lines,
        reason = "construction lists every app-owned state field explicitly so local, shared, and transient state remain auditable"
    )]
    pub fn with_content(content: ContentStore) -> Self {
        let (analysis_result_tx, analysis_rx) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
        // In memory until `new_persistent` swaps in the per-user preview cache:
        // tests and offscreen shots must not write the user's cache.
        let thumbnail_service = with_thumbnail_wake(thumbnail::ThumbnailService::ephemeral(1));
        let pending: Rc<Cell<Option<Hit>>> = Rc::new(Cell::new(None));
        let metadata_editor_pending: Rc<Cell<Option<MetadataEditorHit>>> = Rc::new(Cell::new(None));
        let collection_page_pending: Rc<Cell<Option<CollectionPageHit>>> = Rc::new(Cell::new(None));
        let people_pending = Rc::new(RefCell::new(None));
        let library_analysis_pending: Rc<Cell<Option<LibraryAnalysisHit>>> =
            Rc::new(Cell::new(None));
        let directory_import_pending: Rc<Cell<Option<DirectoryStatusHit>>> =
            Rc::new(Cell::new(None));
        let chrome = chrome::Chrome::new(&pending);

        let initial_analysis_capability = Self::discover_library_analysis_capability().ok();
        Self {
            dispatch: None,
            opens: Rc::default(),
            actions: None,
            app_name: tr("photos-app"),
            content,
            #[cfg(test)]
            last_library_action: None,
            original: None,
            original_identity: None,
            observed_content: None,
            import_content: None,
            picker_stale: false,
            rendered: None,
            viewer: None,
            viewer_details: None,
            chrome,
            layout_theme: Theme::dark(),
            pending,
            notice: None,
            status_bubble: ui::StatusBubble::new(),
            empty_state: None,
            empty_state_key: None,
            rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            picker: None,
            contents_picker: None,
            directory_picker: None,
            directory_picker_retain_exif_location: true,
            directory_import: None,
            directory_import_receipt: None,
            directory_import_details_open: false,
            directory_import_details_scroll: ui::widget::ScrollRegion::vertical(),
            directory_import_details_layout: None,
            directory_import_buttons: build_directory_import_buttons(None),
            directory_import_pending,
            directory_import_progress: ProgressBar::indeterminate()
                .with_label(tr("photos-import-progress-label")),
            resource_invocation: None,
            replies: HashMap::new(),
            #[cfg(test)]
            settled: Vec::new(),
            pending_resource_open: None,
            analysis_tx: None,
            analysis_result_tx,
            analysis_rx,
            active_analysis: HashSet::new(),
            cancelled_analysis: HashSet::new(),
            active_people_scans: HashSet::new(),
            pending_people_scans: HashMap::new(),
            library_analysis: crate::library_analysis::LibraryAnalysisState::default(),
            library_analysis_capability: initial_analysis_capability,
            library_analysis_buttons: build_library_analysis_buttons(None),
            library_analysis_progress: ProgressBar::new(0.0)
                .with_label(tr("photos-analysis-label")),
            library_analysis_pending,
            library_analysis_failure_hits: Vec::new(),
            library_analysis_coverage: (0, 0, 0),
            people_controller: crate::people::PeopleController::new(),
            people_scan_progress: PeopleScanProgress::load(),
            people_mode: false,
            people_selection: None,
            people_detail_transition: Transition::closed(),
            people_merge_mode: false,
            people_show_hidden: false,
            people_scroll: 0.0,
            people_approved: Vec::new(),
            people_delete_confirm: false,
            people_name_input: TextInput::local()
                .with_id("photos-people-name")
                .with_kind(TextInputKind::Name)
                .with_enter_submits(true)
                .with_placeholder(tr("photos-people-name-placeholder")),
            people_name_bounds: Rect::ZERO,
            people_layout: PeopleLayout::default(),
            people_controls: Vec::new(),
            people_control_signature: String::new(),
            people_pending,
            canvas_state: CanvasState::Empty,
            library_index: crate::search::LibraryIndex::empty(),
            search_revision: None,
            search_refresh_error: None,
            #[cfg(test)]
            force_search_refresh_error: false,
            library_items: Vec::new(),
            library_total: 0,
            library_photo_count: 0,
            structure_dirty: false,
            library_destination: LibraryDestination::Timeline,
            places_list_mode: false,
            places_viewport: crate::places::MapViewport::default(),
            places_fit_pending: true,
            places_open_key: None,
            places_items: Vec::new(),
            places_page: 0,
            places_hits: Vec::new(),
            collection_page_hits: Vec::new(),
            collection_page_buttons: build_collection_page_buttons(None),
            collection_page_pending,
            places_map_bounds: Rect::ZERO,
            places_drag_origin: None,
            places_camera_feedback: None,
            places_motion_reduced: false,
            library_favorite_count: 0,
            library_offset: 0,
            library_albums: Vec::new(),
            library_album_covers: HashMap::new(),
            albums_scroll: 0.0,
            albums_page: 0,
            albums_viewport: Rect::ZERO,
            selection_album_page: 0,
            active_album: None,
            active_folder: None,
            active_person: None,
            active_tag: None,
            active_object: None,
            active_pet: None,
            active_favorites: false,
            library_folders: Vec::new(),
            library_people: Vec::new(),
            people_page: 0,
            library_person_covers: HashMap::new(),
            library_tags: Vec::new(),
            metadata_filter_cards: Vec::new(),
            library_page_hits: Vec::new(),
            timeline_density: 1,
            timeline_scroll_y: 0.0,
            timeline_projection: TimelineProjection::default(),
            timeline_projection_revision: None,
            timeline_projection_filter: String::new(),
            timeline_viewport_height: 0.0,
            timeline_index: DateIndexedScrollbar::new("photos:timeline-index"),
            timeline_index_markers: Vec::new(),
            timeline_index_metrics: ScrollMetrics::new(0.0, 0.0, 0.0),
            library_details_cards: Vec::new(),
            album_cards: Vec::new(),
            album_edit_cards: Vec::new(),
            album_editor: false,
            album_editor_id: None,
            album_editor_creating: false,
            album_name_input: TextInput::local().with_id("photos-album-name"),
            album_editor_members: HashSet::new(),
            album_delete_confirm: false,
            album_editor_focus: 0,
            album_editor_rect: Rect::ZERO,
            album_editor_hits: Vec::new(),
            album_editor_pending: Cell::new(None),
            metadata_editor: false,
            metadata_editor_ids: Vec::new(),
            metadata_folder_input: clear_toggled_input(
                "photos-metadata-folder",
                MetadataEditorHit::ClearFolder,
                &metadata_editor_pending,
            ),
            metadata_people_input: clear_toggled_input(
                "photos-metadata-people",
                MetadataEditorHit::ClearPeople,
                &metadata_editor_pending,
            ),
            metadata_tags_input: clear_toggled_input(
                "photos-metadata-tags",
                MetadataEditorHit::ClearTags,
                &metadata_editor_pending,
            ),
            metadata_captured_at_input: clear_toggled_input(
                "photos-metadata-captured-at",
                MetadataEditorHit::ClearCapturedAt,
                &metadata_editor_pending,
            ),
            metadata_place_input: clear_toggled_input(
                "photos-metadata-place",
                MetadataEditorHit::ClearLocation,
                &metadata_editor_pending,
            )
            .with_placeholder(tr("photos-metadata-place-placeholder")),
            metadata_latitude_input: TextInput::local().with_id("photos-metadata-latitude"),
            metadata_longitude_input: TextInput::local().with_id("photos-metadata-longitude"),
            metadata_location_map: crate::places::MapViewport::default(),
            metadata_location_map_bounds: Rect::ZERO,
            metadata_location_from_map: false,
            metadata_mixed: [false; 5],
            metadata_force_clear: [false; 5],
            metadata_editor_section: MetadataEditorSection::Organize,
            metadata_editor_focus: 0,
            metadata_editor_hits: Vec::new(),
            metadata_editor_scroll: 0.0,
            metadata_editor_body: Rect::ZERO,
            metadata_editor_reveal_focus: false,
            metadata_editor_buttons: build_metadata_editor_buttons(None),
            metadata_editor_pending,
            library_thumbnails: HashMap::new(),
            thumbnail_resource_keys: HashMap::new(),
            thumbnail_unavailable: HashSet::new(),
            original_decode_failed: None,
            thumbnail_service,
            thumbnail_keys: HashMap::new(),
            thumbnail_generation: Some(0),
            thumbnail_visible_ids: Vec::new(),
            thumbnail_queued: HashSet::new(),
            library_remove_confirm: None,
            selection_mode: false,
            selected_photo_ids: HashSet::new(),
            selection_anchor: None,
            selection_destination_album: None,
            selection_album_chooser: false,
            selection_album_scroll: ui::widget::ScrollRegion::vertical(),
            selection_all_favorite: false,
            selection_hits: Vec::new(),
            selection_destination_hits: Vec::new(),
            library_cards: Vec::new(),
            pointer: Point::ZERO,
            library_mode: false,
            presented: Rc::default(),
            presented_revision: 0,
            open_photo: None,
            launched: None,
            staged: None,
        }
    }

    /// The local analysis worker's queue, starting the worker if it has not
    /// run yet.
    fn analysis_sender(&mut self) -> Option<SyncSender<AnalysisJob>> {
        if self.analysis_tx.is_none() {
            let (job_tx, job_rx) = std::sync::mpsc::sync_channel(ANALYSIS_QUEUE_LIMIT);
            let results = self.analysis_result_tx.clone();
            let content = self.content.clone();
            self.analysis_tx = ui::rt::spawn_thread("photos-local-analysis", move || {
                analysis_worker(&job_rx, &results, &content);
            })
            .ok()
            .map(|_| job_tx);
        }
        self.analysis_tx.clone()
    }

    /// This app, launched naming `path`: a folder of photographs to import
    /// into the library the window shows, or a photograph the launch
    /// imported, to show.
    #[must_use]
    pub fn launched_on(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.launched = path;
        self
    }

    /// Stage the view state of a screenshot scenario on the next sync, once
    /// its photographs are in the library.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        self.staged = Some(scenario.to_owned());
    }

    /// Import the folder the launch named, or show the photograph it named,
    /// which the launch imported: the photograph its bytes are.
    fn open_launched(&mut self) {
        let Some(path) = self.launched.take() else {
            return;
        };
        if path.is_dir() {
            self.show_library();
            self.begin_directory_import(path, None, DirectoryImportMode::Import, true);
        } else if let Some(bytes) = bounded_file_bytes(&path.to_string_lossy()) {
            let image = OpenImage::from_path(&path.to_string_lossy(), 1, 1)
                .with_content(tilo_content::hash_bytes(&bytes));
            self.show_photo(&LibraryPhoto::from_image(image).id);
        }
    }

    /// Pose the view for `scenario`, whose photographs the scenario's
    /// actions put in the library.
    fn apply_staged(&mut self) {
        let Some(scenario) = self.staged.take() else {
            return;
        };
        self.open_photo = crate::sample::opened(&scenario);
        match scenario.as_str() {
            "albums" => self.show_albums(),
            "selection" => self.show_selection_for_shot(),
            "selection-many-albums" => {
                self.show_selection_for_shot();
                self.show_selection_destination_chooser_for_shot();
            }
            "album-manage" => self.show_album_editor_for_shot(),
            "metadata-edit" => self.show_metadata_editor_for_shot(),
            "metadata-batch" => self.show_batch_metadata_editor_for_shot(),
            "people" | "people-detail" => {
                // A manual, explicitly labelled suggestion exercises the
                // review layout without pretending that a face model
                // recognized anyone. Live windows never stage People data.
                let _ = self.people_controller.add_cluster(
                    "shots-imported-suggestion",
                    crate::sample::people_suggestion(),
                    crate::people::ProposalOrigin::ImportedOrManual,
                );
                if scenario == "people-detail" {
                    self.show_people_suggestion_for_shot("shots-imported-suggestion");
                } else {
                    self.show_people();
                }
            }
            "places" => self.show_places_for_shot(),
            "analysis" => self.show_library_analysis(),
            "import-progress" => self.show_directory_import_progress_for_shot(),
            "import-errors" => self.show_directory_import_errors_for_shot(),
            scenario if crate::sample::shows_library(scenario) => self.show_library(),
            _ => {}
        }
    }

    /// Import `path` through the canonical durable action. The decoded source
    /// path remains only in this process; the action writes portable metadata
    /// and the staged content hash into the shared document.
    pub fn open_path(&mut self, path: &str) {
        self.open_bytes(path, bounded_file_bytes(path).as_deref());
    }

    /// Import a photograph chosen by its contents, where it has no path: its
    /// name labels it and its bytes live in the content store.
    pub fn open_contents(&mut self, name: &str, bytes: &[u8]) {
        let bytes = (bytes.len() <= pixels::MAX_ENCODED_BYTES).then_some(bytes);
        self.open_bytes(name, bytes);
    }

    fn open_bytes(&mut self, path: &str, bytes: Option<&[u8]>) {
        match bytes.and_then(pixels::decode) {
            Some(img) => {
                // Put the bytes in the content store before naming them, so
                // the hash this document is about to carry is one this machine
                // can already answer for — and one the room can be handed.
                // Otherwise the document would name a picture only the path
                // could find, and the path is true on one machine.
                let content = match self.content.put(bytes.unwrap_or_default()) {
                    Ok(content) => content,
                    Err(error) => {
                        tracing::warn!(%error, "photos: could not store an opened original");
                        self.notice = Some(tr("photos-open-failed"));
                        return;
                    }
                };
                let open = OpenImage::from_path(path, img.w, img.h).with_content(content);
                self.original = Some((path.to_string(), img));
                self.original_identity = Some(decode_key(&open.clone().portable()));
                self.rendered = None;
                self.close_viewer();
                self.original_decode_failed = None;
                let photo = LibraryPhoto::from_image(open);
                let photo_id = photo.id.clone();
                self.commit(crate::resource::import(&photo));
                self.show_photo(&photo_id);
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

    /// The header row's rect.
    pub fn toolbar_rect(&self) -> Rect {
        self.chrome.header
    }

    /// The status bar's rect; zero height when it has nothing to show.
    pub fn status_rect(&self) -> Rect {
        self.status_rect
    }

    /// Every control of the header row as `(accessible name, rect)`, in
    /// order: the back button while a photograph is open, the commands the
    /// toolbar shows (and its More button when some moved there) and the
    /// search launcher.
    pub fn toolbar_item_rects(&self) -> Vec<(String, Rect)> {
        let mut items = Vec::new();
        if self.header_back_visible() {
            items.push((tr("photos-library"), self.chrome.back.get_rect()));
        }
        if let Some(toolbar) = self.chrome.toolbar.accessibility() {
            for node in toolbar.children {
                if !matches!(node.role, Role::Button | Role::ToggleButton) {
                    continue;
                }
                items.push((
                    node.name.unwrap_or_default(),
                    node.bounds
                        .map_or(Rect::ZERO, ui::a11y::AccessBounds::to_rect),
                ));
            }
        }
        items.push((
            self.chrome
                .search
                .accessibility()
                .and_then(|node| node.name)
                .unwrap_or_default(),
            self.chrome.search.get_rect(),
        ));
        items
    }

    /// Measured library-card geometry for screenshot and interaction
    /// regressions. Every returned tuple is `(card, details, remove,
    /// favorite)`; the three controls are their actual 44px hit rectangles,
    /// not the smaller icon artwork inset inside them.
    pub fn library_card_action_rects(&self) -> Vec<(Rect, Rect, Rect, Rect)> {
        self.library_cards
            .iter()
            .filter(|(id, _, _)| {
                self.library_details_cards
                    .iter()
                    .any(|(candidate, _)| candidate == id)
            })
            .map(|(_, card, _)| {
                let actions = card_action_controls(*card, &self.layout_theme);
                (*card, actions.details, actions.remove, actions.favorite)
            })
            .collect()
    }

    /// The status bar as it currently reads.
    pub fn status(&self) -> String {
        [&self.chrome.status_left, &self.chrome.status_right]
            .into_iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether the library, rather than one photograph, is shown.
    pub fn shows_library(&self) -> bool {
        self.library_mode
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

    /// Runtime constructor: photographs in the app's `content` store and the
    /// device-local People review queue. Tests use [`Self::new`] so they never
    /// read or mutate the signed-in user's private recognition state.
    #[must_use]
    pub fn new_persistent(content: ContentStore) -> Self {
        let mut app = Self::with_content(content);
        app.people_controller = crate::people::PeopleController::persistent();
        if let Ok(service) = thumbnail::ThumbnailService::open_default(1) {
            app.thumbnail_service = with_thumbnail_wake(service);
        }
        app
    }

    /// Mutable local People controller. Callers use this for explicit opt-in,
    /// review, hiding, splitting, and deletion of private clusters.
    pub fn people_controller_mut(&mut self) -> &mut crate::people::PeopleController {
        &mut self.people_controller
    }

    /// Commit a reviewed cluster's compact, user-approved membership through
    /// the library's verb. The local cluster stays local; the
    /// action contains only the selected display name and stable photo ids.
    pub fn approve_people_cluster(&mut self, cluster_id: &str) -> bool {
        let Some(membership) = self.people_controller.approve_membership(cluster_id) else {
            self.notice = Some(tr("photos-people-confirm-first"));
            return false;
        };
        self.commit(PhotoLibrary::SET_PERSON_MEMBERSHIP.with(membership));
        self.notice =
            Some("Approved person labels were added to the shared photo library".to_owned());
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "rebuilds every retained People control from one cluster snapshot so labels and routes cannot drift"
    )]
    fn refresh_people_controls(&mut self) {
        let clusters: Vec<_> = self
            .people_controller
            .all_clusters()
            .map(|cluster| {
                (
                    cluster.id.clone(),
                    cluster.display_name.clone(),
                    cluster.photos.len(),
                    cluster.hidden,
                    cluster.rejected,
                )
            })
            .collect();
        let signature = format!(
            "{:?}|{}|{}|{}|{}|{}|{:?}|{:?}",
            self.people_selection,
            self.people_merge_mode,
            self.people_show_hidden,
            self.people_delete_confirm,
            self.active_people_scans.is_empty(),
            self.people_controller.privacy().recognition_enabled,
            self.people_approved
                .iter()
                .map(|person| (&person.label, person.count()))
                .collect::<Vec<_>>(),
            clusters
        );
        if signature == self.people_control_signature {
            return;
        }
        self.people_control_signature = signature;
        self.people_controls.clear();
        let actions = self.actions.clone();
        let recognition_enabled = self.people_controller.privacy().recognition_enabled;
        let mut push = |intent: PeopleIntent, label: String, primary: bool, danger: bool| {
            let button = Button::new(
                &format!("photos-people-control-{}", self.people_controls.len()),
                &label,
            )
            .with_accessible_name(&label);
            let queued = intent.clone();
            let pending = Rc::clone(&self.people_pending);
            let button = if matches!(
                &intent,
                PeopleIntent::OpenApproved(_)
                    | PeopleIntent::OpenSuggestion(_)
                    | PeopleIntent::BrowseApproved
                    | PeopleIntent::BeginMerge
            ) {
                button.on_view(move || *pending.borrow_mut() = Some(queued.clone()))
            } else {
                button.on_effect(move || *pending.borrow_mut() = Some(queued.clone()))
            };
            let mut button = bound(button, &intent.effect(), actions.as_ref());
            if primary {
                button = button.role(ButtonRole::Primary);
            } else if danger
                || matches!(
                    &intent,
                    PeopleIntent::DeleteLocalData
                        | PeopleIntent::ConfirmDeleteLocalData
                        | PeopleIntent::RejectSuggestion
                )
            {
                button = button.role(ButtonRole::Destructive);
            }
            if matches!(intent, PeopleIntent::Scan) && !recognition_enabled {
                button = button.with_enabled(false);
            }
            // The width is the button's own measure, read back from the
            // layout pass that places it.
            self.people_controls.push(PeopleControl {
                intent,
                label,
                bounds: Rect::ZERO,
                button,
                width: 0.0,
            });
        };

        if self.people_selection.is_none() {
            let recognition_on = self.people_controller.privacy().recognition_enabled;
            push(
                PeopleIntent::ToggleRecognition,
                if recognition_on {
                    tr("photos-people-recognition-off")
                } else {
                    tr("photos-people-recognition-on")
                },
                false,
                false,
            );
            if self.active_people_scans.is_empty() {
                push(PeopleIntent::Scan, tr("photos-people-scan"), true, false);
            } else {
                push(
                    PeopleIntent::CancelScan,
                    tr("photos-people-pause-scan"),
                    false,
                    false,
                );
            }
            push(
                if self.people_delete_confirm {
                    PeopleIntent::ConfirmDeleteLocalData
                } else {
                    PeopleIntent::DeleteLocalData
                },
                if self.people_delete_confirm {
                    tr("photos-people-confirm-delete-data")
                } else {
                    tr("photos-people-delete-data")
                },
                false,
                self.people_delete_confirm,
            );
            for person in &self.people_approved {
                push(
                    PeopleIntent::OpenApproved(person.label.clone()),
                    tr_args(
                        "photos-people-card-name",
                        &[
                            ("name", ArgValue::Text(person.label.clone())),
                            ("count", ArgValue::Number(person.count() as f64)),
                        ],
                    ),
                    false,
                    false,
                );
            }
            let visible_clusters: Vec<_> = self
                .people_controller
                .all_clusters()
                .filter(|cluster| self.people_show_hidden || (!cluster.hidden && !cluster.rejected))
                .map(|cluster| {
                    (
                        cluster.id.clone(),
                        cluster.display_name.clone(),
                        cluster.photos.len(),
                        cluster.hidden || cluster.rejected,
                    )
                })
                .collect();
            for (id, name, count, hidden) in visible_clusters {
                push(
                    PeopleIntent::OpenSuggestion(id),
                    tr_args(
                        if hidden {
                            "photos-people-card-hidden"
                        } else {
                            "photos-people-card-review"
                        },
                        &[
                            (
                                "name",
                                ArgValue::Text(
                                    name.unwrap_or_else(|| tr("photos-people-suggested")),
                                ),
                            ),
                            ("count", ArgValue::Number(count as f64)),
                        ],
                    ),
                    false,
                    false,
                );
            }
        } else {
            match self.people_selection.clone() {
                Some(PeopleSelection::Approved(name)) => {
                    if self.people_merge_mode {
                        for person in &self.people_approved {
                            if person.label != name {
                                push(
                                    PeopleIntent::MergeApprovedInto(person.label.clone()),
                                    tr_with("photos-people-merge-into", "name", &person.label),
                                    false,
                                    false,
                                );
                            }
                        }
                    } else {
                        push(
                            PeopleIntent::BrowseApproved,
                            tr("photos-people-browse"),
                            true,
                            false,
                        );
                        push(
                            PeopleIntent::SaveName,
                            tr("photos-people-save-name"),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::BeginMerge,
                            tr("photos-people-merge"),
                            false,
                            false,
                        );
                    }
                }
                Some(PeopleSelection::Suggestion(id)) => {
                    let hidden = self
                        .people_controller
                        .cluster(&id)
                        .is_some_and(|cluster| cluster.hidden || cluster.rejected);
                    if self.people_merge_mode {
                        let candidates: Vec<_> = self
                            .people_controller
                            .all_clusters()
                            .filter(|cluster| cluster.id != id && !cluster.rejected)
                            .map(|cluster| {
                                (
                                    cluster.id.clone(),
                                    cluster
                                        .display_name
                                        .clone()
                                        .unwrap_or_else(|| tr("photos-people-suggested")),
                                )
                            })
                            .collect();
                        for (candidate, name) in candidates {
                            push(
                                PeopleIntent::MergeSuggestionWith(candidate),
                                tr_with("photos-people-combine-with", "name", &name),
                                false,
                                false,
                            );
                        }
                    } else if hidden {
                        push(
                            PeopleIntent::RestoreSuggestion,
                            tr("photos-people-restore"),
                            true,
                            false,
                        );
                    } else {
                        push(
                            PeopleIntent::ApproveSuggestion,
                            tr("photos-people-approve"),
                            true,
                            false,
                        );
                        push(
                            PeopleIntent::HideSuggestion,
                            tr("photos-people-decide-later"),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::RejectSuggestion,
                            tr("photos-people-dismiss"),
                            false,
                            false,
                        );
                        push(
                            PeopleIntent::BeginMerge,
                            tr("photos-people-combine"),
                            false,
                            false,
                        );
                    }
                }
                None => {}
            }
        }
    }

    fn select_people_item(&mut self, selection: PeopleSelection) {
        let initial = match &selection {
            PeopleSelection::Approved(name) => name.clone(),
            PeopleSelection::Suggestion(id) => self
                .people_controller
                .cluster(id)
                .and_then(|cluster| cluster.display_name.clone())
                .unwrap_or_default(),
        };
        self.people_selection = Some(selection);
        self.people_detail_transition
            .set_open(true, Duration::from_millis(190), Curve::EaseOut);
        self.people_merge_mode = false;
        self.people_delete_confirm = false;
        self.people_name_input.set_text(&initial);
        self.people_name_input.set_focus(false);
        self.people_control_signature.clear();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match routes every People intent to its local or shared effect"
    )]
    fn handle_people_intent(&mut self, intent: PeopleIntent) {
        match intent {
            PeopleIntent::Back => {
                if self.people_selection.is_some() {
                    self.people_selection = None;
                    self.people_merge_mode = false;
                    self.people_detail_transition.set_open(
                        false,
                        Duration::from_millis(160),
                        Curve::EaseOut,
                    );
                } else {
                    self.library_destination = LibraryDestination::Timeline;
                    self.people_mode = false;
                }
            }
            PeopleIntent::ToggleRecognition => {
                let enabled = !self.people_controller.privacy().recognition_enabled;
                self.people_controller.set_recognition_enabled(enabled);
                if enabled {
                    self.people_controller
                        .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                }
                self.notice = Some(self.people_controller.capability().summary());
            }
            PeopleIntent::Scan => {
                let invocation = tilo_actions::InvocationId::new();
                if let Err(error) = self.schedule_people_scan_batch(invocation, &self.presented()) {
                    self.notice = Some(error);
                } else {
                    self.notice = Some(tr("photos-people-scanning-notice"));
                }
            }
            PeopleIntent::CancelScan => {
                for invocation in self.active_people_scans.drain() {
                    self.active_analysis.remove(&invocation);
                    self.cancelled_analysis.insert(invocation);
                    self.pending_people_scans.remove(&invocation);
                }
                "cancelled".clone_into(&mut self.people_scan_progress.status);
                self.people_scan_progress.save();
                self.notice = Some(tr("photos-people-scan-paused"));
            }
            PeopleIntent::OpenApproved(name) => {
                self.select_people_item(PeopleSelection::Approved(name));
            }
            PeopleIntent::OpenSuggestion(id) => {
                self.select_people_item(PeopleSelection::Suggestion(id));
            }
            PeopleIntent::BrowseApproved => {
                if let Some(PeopleSelection::Approved(name)) = &self.people_selection {
                    self.active_person = Some(name.clone());
                    self.library_destination = LibraryDestination::Timeline;
                    self.people_mode = false;
                    self.timeline_scroll_y = 0.0;
                }
            }
            PeopleIntent::SaveName => {
                if let Some(PeopleSelection::Approved(from)) = self.people_selection.clone() {
                    let to = self.people_name_input.value().trim().to_owned();
                    if to.is_empty() {
                        self.notice = Some(tr("photos-people-name-first"));
                    } else {
                        self.commit(PhotoLibrary::RENAME_PERSON.with((from, to.clone())));
                        self.people_selection = Some(PeopleSelection::Approved(to));
                        self.notice = Some(tr("photos-people-renamed"));
                    }
                }
            }
            PeopleIntent::BeginMerge => self.people_merge_mode = true,
            PeopleIntent::MergeApprovedInto(to) => {
                if let Some(PeopleSelection::Approved(from)) = self.people_selection.clone() {
                    self.commit(PhotoLibrary::RENAME_PERSON.with((from, to)));
                    self.people_selection = None;
                    self.people_merge_mode = false;
                    self.notice = Some(tr("photos-people-merged"));
                }
            }
            PeopleIntent::ApproveSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let name = self.people_name_input.value().trim().to_owned();
                    if matches!(
                        self.people_controller.confirm_suggestion(&id, &name),
                        crate::people::PeopleChange::Unchanged
                    ) || !self.approve_people_cluster(&id)
                    {
                        self.notice = Some(tr("photos-people-name-before-approving"));
                    } else {
                        let _ = self.people_controller.set_hidden(&id, true);
                        self.people_selection = None;
                    }
                }
            }
            PeopleIntent::RejectSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.reject_suggestion(&id);
                    self.people_selection = None;
                    self.notice = Some(tr("photos-people-dismissed"));
                }
            }
            PeopleIntent::HideSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.set_hidden(&id, true);
                    self.people_selection = None;
                    self.notice = Some(tr("photos-people-hidden"));
                }
            }
            PeopleIntent::RestoreSuggestion => {
                if let Some(PeopleSelection::Suggestion(id)) = self.people_selection.clone() {
                    let _ = self.people_controller.restore_suggestion(&id);
                    self.people_selection = None;
                    self.notice = Some(tr("photos-people-restored"));
                }
            }
            PeopleIntent::MergeSuggestionWith(source) => {
                if let Some(PeopleSelection::Suggestion(destination)) =
                    self.people_selection.clone()
                {
                    let _ = self.people_controller.merge_clusters(&destination, &source);
                    self.people_merge_mode = false;
                    self.notice = Some(tr("photos-people-combined"));
                }
            }
            PeopleIntent::Page(forward) => {
                let step = self.people_layout.lanes.height();
                let max = (self.people_layout.lanes_content - step).max(0.0);
                self.people_scroll =
                    (self.people_scroll + if forward { step } else { -step }).clamp(0.0, max);
            }
            PeopleIntent::ToggleHidden => {
                self.people_show_hidden = !self.people_show_hidden;
                self.people_scroll = 0.0;
            }
            PeopleIntent::DeleteLocalData => {
                self.people_delete_confirm = true;
                self.notice = Some(tr("photos-people-confirm-delete-notice"));
            }
            PeopleIntent::ConfirmDeleteLocalData => {
                self.people_controller.delete_all_local_biometric_data();
                self.people_delete_confirm = false;
                self.people_selection = None;
                self.notice = Some(tr("photos-people-deleted-data"));
            }
        }
        self.people_control_signature.clear();
    }

    /// Drop the open photograph's viewer. The layout tree points at it, so
    /// dropping one that existed asks for a fresh tree.
    fn close_viewer(&mut self) {
        if self.viewer.take().is_some() {
            self.structure_dirty = true;
        }
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
        self.timeline_scroll_y = 0.0;
        self.library_remove_confirm = None;
        self.finish_selection();
    }

    /// Resolve a canonical timeline resource without treating a visual card as
    /// the source of truth. The shared capture value is the only input: a
    /// removed or changed boundary refuses instead of jumping to a nearby,
    /// misleading date.
    fn view_timeline(
        &mut self,
        library: &crate::model::PhotoLibrary,
        anchor: Option<&crate::actions::TimelineAnchor>,
    ) -> Result<(), String> {
        self.show_library();
        let Some(anchor) = anchor else {
            return Ok(());
        };
        let mut photos: Vec<_> = library
            .photos
            .values()
            .cloned()
            .map(|photo| {
                let displayed_day = photo
                    .captured_at
                    .as_deref()
                    .map_or_else(|| "Undated".to_owned(), capture_day);
                let canonical_day = photo
                    .captured_at
                    .as_deref()
                    .and_then(crate::derived::timeline_day_from_capture);
                (displayed_day, canonical_day, photo.id)
            })
            .collect();
        photos.sort_by(|(left_day, _, left_id), (right_day, _, right_id)| {
            let left_undated = left_day == "Undated";
            let right_undated = right_day == "Undated";
            left_undated
                .cmp(&right_undated)
                .then_with(|| right_day.cmp(left_day))
                .then_with(|| left_id.cmp(right_id))
        });
        let target = photos.iter().find(|(_, canonical_day, _)| match anchor {
            crate::actions::TimelineAnchor::Month { month } => canonical_day
                .as_deref()
                .is_some_and(|day| day.starts_with(month)),
            crate::actions::TimelineAnchor::Day { day: wanted } => {
                canonical_day.as_deref() == Some(wanted)
            }
        });
        let Some((target_day, _, _)) = target else {
            return Err(tr("photos-error-day-gone"));
        };
        if let Some(offset) = self
            .timeline_projection
            .day_offset(target_day, self.timeline_viewport_height)
        {
            self.timeline_scroll_y = offset;
            return Ok(());
        }
        // A semantic action can arrive before the first painted frame. Build
        // the same lightweight metadata projection now so the jump is exact
        // rather than falling back to a guessed fixed-height row.
        self.timeline_projection
            .replace(library.photos.values().map(|photo| {
                TimelinePhoto {
                    id: photo.id.clone(),
                    name: photo.image.name.clone(),
                    favorite: photo.favorite,
                    day: photo
                        .captured_at
                        .as_deref()
                        .map_or_else(|| "Undated".to_owned(), capture_day),
                    captured_at: photo.captured_at.clone().unwrap_or_default(),
                    aspect_ratio: if photo.image.width > 0 && photo.image.height > 0 {
                        f64::from(photo.image.width) / f64::from(photo.image.height)
                    } else {
                        1.5
                    },
                }
            }));
        let geometry = self.timeline_geometry(&self.layout_theme.clone());
        self.timeline_projection.set_geometry(geometry);
        self.timeline_scroll_y = self
            .timeline_projection
            .day_offset(target_day, self.timeline_viewport_height)
            .unwrap_or(0.0);
        Ok(())
    }

    /// Reveal the People collection without enabling recognition or changing
    /// any document metadata. This is used by the resource action and visual
    /// acceptance scenarios.
    pub fn show_people(&mut self) {
        self.show_library();
        self.people_mode = true;
        self.library_destination = LibraryDestination::People;
    }

    /// Reveal albums as a first-class collection. Opening the collection is
    /// presentation-only; opening a specific album still resolves its stable
    /// resource id and then enters the chronological photo view.
    pub fn show_albums(&mut self) {
        self.show_library();
        self.people_mode = false;
        self.library_destination = LibraryDestination::Albums;
    }

    /// Pose one real local suggestion for the deterministic visual gallery.
    /// The named cluster must already exist; this never invents recognition
    /// output or runs on a live launch.
    pub fn show_people_suggestion_for_shot(&mut self, cluster_id: &str) {
        self.show_people();
        if self.people_controller.cluster(cluster_id).is_some() {
            self.select_people_item(PeopleSelection::Suggestion(cluster_id.to_owned()));
            // Acceptance shots exercise the settled visual state. Live entry
            // still starts the shared transition in `select_people_item`.
            self.people_detail_transition = Transition::open();
        }
    }

    /// Reveal the whole-library local-analysis dashboard for deterministic
    /// visual acceptance. No scan is started and no result is fabricated.
    pub fn show_library_analysis(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Analysis;
        self.library_analysis_capability = Self::discover_library_analysis_capability().ok();
    }

    fn begin_selection(&mut self) {
        self.selection_mode = true;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_scroll.set_offset(Vec2::ZERO);
        self.selection_all_favorite = false;
        self.notice = Some(tr("photos-selection-hint"));
    }

    fn finish_selection(&mut self) {
        self.selection_mode = false;
        self.selected_photo_ids.clear();
        self.selection_anchor = None;
        self.selection_destination_album = None;
        self.selection_album_chooser = false;
        self.selection_album_scroll.set_offset(Vec2::ZERO);
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

    fn set_selected_favorite(&mut self, favorite: bool) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some(tr("photos-selection-empty"));
            return;
        }
        let edits: Vec<_> = ids
            .iter()
            .map(|id| crate::model::photo_paths(id).favorite.set(favorite))
            .collect();
        self.commit_group(&edits);
        self.notice = Some(if favorite {
            tr("photos-selection-favorited")
        } else {
            tr("photos-selection-unfavorited")
        });
        self.finish_selection();
    }

    fn add_selected_to_destination_album(&mut self) {
        let Some(album_id) = self.selection_destination_album.clone() else {
            self.notice = Some(tr("photos-selection-choose-album"));
            return;
        };
        let library = self.presented();
        let Some(album) = library.album(&album_id) else {
            self.notice = Some(tr("photos-album-gone"));
            return;
        };
        let mut selected: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        selected.sort_unstable();
        if selected.is_empty() {
            self.notice = Some(tr("photos-selection-empty"));
            return;
        }
        // Each photograph filed on its own, so a collaborator filing others
        // meanwhile keeps theirs.
        let paths = crate::model::album_paths(&album_id).photo_ids;
        let edits: Vec<_> = selected
            .iter()
            .filter(|id| !album.photo_ids.contains(id))
            .map(|id| paths.insert(id))
            .collect();
        self.commit_group(&edits);
        self.notice = Some("Selected photographs added to the album".to_owned());
        self.finish_selection();
    }

    fn handle_selection_hit(&mut self, hit: SelectionHit) {
        match hit {
            SelectionHit::Start => self.begin_selection(),
            SelectionHit::SelectAll => self.select_all_visible(),
            SelectionHit::Clear => {
                self.selected_photo_ids.clear();
                self.selection_anchor = None;
            }
            SelectionHit::Favorite => {
                let library = self.presented();
                let all_favorite = !self.selected_photo_ids.is_empty()
                    && self
                        .selected_photo_ids
                        .iter()
                        .all(|id| library.photo(id).is_some_and(|photo| photo.favorite));
                self.set_selected_favorite(!all_favorite);
            }
            SelectionHit::EditDetails => self.begin_selected_metadata_editor(),
            SelectionHit::ChooseAlbum => {
                if self.selected_photo_ids.is_empty() || self.library_albums.is_empty() {
                    self.notice = Some(tr("photos-selection-need-album"));
                } else {
                    self.selection_album_chooser = true;
                    self.selection_album_scroll.set_offset(Vec2::ZERO);
                }
            }
            SelectionHit::CancelDestinationChooser | SelectionHit::CloseDestinationChooser => {
                self.selection_album_chooser = false;
            }
            SelectionHit::ApplyDestination => {
                if self.selection_destination_album.is_some() {
                    self.selection_album_chooser = false;
                } else {
                    self.notice = Some(tr("photos-selection-choose-album"));
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
            SelectionHit::AddToAlbum => self.add_selected_to_destination_album(),
            SelectionHit::Done => self.finish_selection(),
        }
    }

    /// Pose the ordinary library editor for deterministic offscreen coverage.
    /// The production entry point is the visible “Edit album” control; shot
    /// rendering has no window event stream to click it through.
    pub fn show_album_editor_for_shot(&mut self) {
        self.show_library();
        self.active_album = self
            .presented()
            .albums
            .values()
            .next()
            .map(|album| album.id.clone());
        self.begin_edit_album();
    }

    /// Pose the shared metadata editor for visual regression coverage.
    pub fn show_metadata_editor_for_shot(&mut self) {
        self.show_library();
        let library = self.presented();
        if let Some(id) = library
            .photos
            .values()
            .find(|photo| !photo.metadata_summary().is_empty())
            .or_else(|| library.photos.values().next())
            .map(|photo| photo.id.clone())
        {
            self.begin_metadata_editor(&id);
        }
    }

    /// Pose a mixed-value multi-photo edit on its location section. This is
    /// the state most likely to regress silently because blank means “keep”
    /// until the user explicitly clears or replaces the dimension.
    pub fn show_batch_metadata_editor_for_shot(&mut self) {
        self.show_selection_for_shot();
        self.begin_selected_metadata_editor();
        self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
    }

    /// Pose the offline Places destination for deterministic visual coverage.
    pub fn show_places_for_shot(&mut self) {
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_open_key = None;
        self.places_fit_pending = true;
    }

    /// Apply a local map-camera change and give the marker layer a restrained
    /// settle cue. The viewport is presentation state, so the animation never
    /// delays a semantic action or writes shared metadata.
    fn set_places_viewport(&mut self, viewport: crate::places::MapViewport) {
        let viewport = viewport.clamped();
        if self.places_viewport == viewport {
            return;
        }
        self.places_viewport = viewport;
        if !self.places_motion_reduced {
            self.places_camera_feedback = Some(Animated::new(
                1.0,
                0.0,
                Duration::from_millis(180),
                Curve::EaseOut,
            ));
        }
    }

    fn zoom_places_by(&mut self, delta: f64) {
        self.set_places_viewport(crate::places::MapViewport {
            zoom: self.places_viewport.zoom + delta,
            ..self.places_viewport
        });
    }

    fn places_camera_feedback(&mut self) -> f64 {
        let Some(feedback) = self.places_camera_feedback else {
            return 0.0;
        };
        let value = feedback.current(ui::time::now());
        if feedback.done(ui::time::now()) {
            self.places_camera_feedback = None;
            0.0
        } else {
            value
        }
    }

    /// Open the exact durable collection behind a Places resource or marker.
    /// This reads only shared metadata, so it remains usable offline and
    /// deliberately fails if a collaborator removed the addressed place.
    fn open_place_collection(
        &mut self,
        library: &crate::model::PhotoLibrary,
        place_key: &str,
    ) -> Result<(), String> {
        let photos: Vec<_> = library
            .photos
            .values()
            .filter(|photo| {
                photo.location.as_ref().is_some_and(|location| {
                    location.is_valid() && location.place_key() == place_key
                })
            })
            .cloned()
            .collect();
        let Some(location) = photos.first().and_then(|photo| photo.location.as_ref()) else {
            return Err(tr("photos-error-place-gone"));
        };
        self.show_library();
        self.library_destination = LibraryDestination::Places;
        self.places_open_key = Some(place_key.to_owned());
        self.places_list_mode = true;
        self.places_page = 0;
        self.set_places_viewport(crate::places::MapViewport {
            latitude: location.latitude,
            longitude: location.longitude,
            zoom: 12.0,
        });
        Ok(())
    }

    /// Pose a running folder import for hermetic offscreen visual coverage.
    /// The held sender prevents the ordinary sync loop from treating this
    /// deterministic fixture as a crashed worker.
    pub fn show_directory_import_progress_for_shot(&mut self) {
        self.show_library();
        let (sender, receiver) = std::sync::mpsc::channel();
        self.directory_import_receipt = None;
        self.directory_import_details_open = false;
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            invocation: None,
            mode: DirectoryImportMode::Import,
            discovered: 184,
            imported: 112,
            refreshed: 0,
            unchanged: 7,
            skipped: 2,
            sequence: 184,
            pending: VecDeque::new(),
            failures: vec![
                crate::import::ImportFailure {
                    source: "Scan of album page 3.tif".into(),
                    message: tr("photos-import-unsafe"),
                    retryable: true,
                },
                crate::import::ImportFailure {
                    source: "Old phone backup".into(),
                    message: tr("photos-import-link"),
                    retryable: false,
                },
            ],
            worker_done: false,
            cancelled: false,
            limit_reached: false,
            produced_resources: Vec::new(),
            _shot_sender: Some(sender),
        });
    }

    /// Pose a completed partial import with a deliberately bounded issue
    /// receipt. It exists only for offscreen regression images.
    pub fn show_directory_import_errors_for_shot(&mut self) {
        self.show_library();
        self.directory_import = None;
        let failures = [
            ("IMG_2041.JPG", true),
            ("IMG_2047.JPG", true),
            ("DSC01183.ARW", false),
            ("IMG_2103.JPG", true),
            ("Beach panorama.tif", false),
            ("IMG_2131.JPG", true),
            ("DSC01207.ARW", false),
            ("Scan of album page 3.tif", true),
            ("IMG_2188.JPG", true),
        ]
        .into_iter()
        .map(|(name, retryable)| crate::import::ImportFailure {
            source: name.into(),
            message: if retryable {
                tr("photos-import-unreadable")
            } else {
                tr("photos-import-unsafe")
            },
            retryable,
        })
        .collect();
        self.directory_import_receipt = Some(DirectoryImportReceipt {
            mode: DirectoryImportMode::Import,
            discovered: 268,
            imported: 241,
            refreshed: 0,
            unchanged: 0,
            failures,
            failure_count: 27,
            cancelled: false,
            limit_reached: false,
        });
        self.directory_import_details_open = true;
        self.directory_import_details_scroll.set_offset(Vec2::ZERO);
    }

    /// Pose the ordinary selection shelf for deterministic visual coverage.
    /// Two real photographs are selected so the capture exercises the active
    /// controls and selected-card treatment, not merely their disabled state.
    pub fn show_selection_for_shot(&mut self) {
        self.show_library();
        self.begin_selection();
        for photo in self.presented().all_photos().take(2) {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id.clone());
        }
    }

    /// Pose a deliberately long destination list. The selection is local;
    /// this exists only to make the scrolling list visible in the
    /// deterministic accessibility and responsive screenshots.
    pub fn show_selection_destination_chooser_for_shot(&mut self) {
        if let Some(photo) = self.presented().photos.values().next() {
            self.selected_photo_ids.insert(photo.id.clone());
            self.selection_anchor = Some(photo.id.clone());
        }
        self.selection_album_chooser = true;
        self.selection_album_scroll.set_offset(Vec2::ZERO);
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
        let device_id = winit::event::DeviceId::dummy();
        v.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id,
                position: center,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        v.handle_event(
            &winit::event::WindowEvent::MouseWheel {
                device_id,
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, scroll_y),
                phase: winit::event::TouchPhase::Moved,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// The visible region of the canvas in normalized image coordinates, or
    /// the whole image when the viewer has not been laid out yet.
    ///
    /// `ImageViewer` exposes no direct query for this either, so it is derived
    /// in `visible_image_rect` from the same "contain fit, then zoom and
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
        let saved = tilo_files::file_at(std::path::Path::new(path))
            .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes));
        if saved.is_ok() {
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

    /// Submit `edit` of the photograph this window shows, as an undoable
    /// change of the library.
    fn edit_shown<S: ui::framework::Submit<PhotoLibrary>>(&mut self, edit: impl FnOnce(Id) -> S) {
        let Some(photo_id) = self.open_photo.clone() else {
            return;
        };
        if let Err(error) = self.submit(edit(Id::from_raw(photo_id))) {
            tracing::warn!(%error, "photos: the edit was refused");
        }
        self.notice = None;
    }

    /// Show photograph `photo_id` in this window.
    fn show_photo(&mut self, photo_id: &str) {
        self.open_photo = Some(photo_id.to_owned());
        self.library_mode = false;
    }

    /// The photograph this window shows, as the library holds it now.
    pub(crate) fn shown_photo(&self) -> Option<LibraryPhoto> {
        self.shown_photo_in(&self.presented())
    }

    /// The library as the framework last presented it: what every read of
    /// the widget answers from, so it draws exactly the state its resource
    /// holds, the live one or a past version.
    fn presented(&self) -> Rc<crate::model::PhotoLibrary> {
        Rc::clone(&self.presented)
    }

    /// The photograph this window shows, in `library`.
    fn shown_photo_in(&self, library: &crate::model::PhotoLibrary) -> Option<LibraryPhoto> {
        library.photo(self.open_photo.as_deref()?).cloned()
    }

    /// The photograph this window shows.
    pub fn open_photo_id(&self) -> Option<&str> {
        self.open_photo.as_deref()
    }

    /// Show the library the framework resource already holds. An empty
    /// projection does not wipe a library that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<crate::model::PhotoLibrary>) {
        if *self.presented == **state {
            return;
        }
        self.presented = Rc::clone(state);
        self.presented_revision = self.presented_revision.wrapping_add(1);
    }

    /// Attach the library's dispatcher, as the framework does when it hosts
    /// the surface.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::model::PhotoLibrary>,
        actions: ui::framework::Actions<crate::model::PhotoLibrary>,
    ) {
        self.dispatch = Some(dispatcher);
        self.actions = Some(actions);
        self.bind_controls();
    }

    /// Build the retained controls again, bound to their effects.
    fn bind_controls(&mut self) {
        self.invalidate_toolbar();
        let actions = self.actions.as_ref();
        self.directory_import_buttons = build_directory_import_buttons(actions);
        self.library_analysis_buttons = build_library_analysis_buttons(actions);
        self.collection_page_buttons = build_collection_page_buttons(actions);
        self.metadata_editor_buttons = build_metadata_editor_buttons(actions);
        // People controls are rebuilt on their next refresh.
        self.people_control_signature.clear();
    }

    /// Answer the effect of a retained control: post its intent for the
    /// next sync, which has the document. `false` when `effect` is not one.
    fn post_control(&self, effect: &PhotosEffect) -> bool {
        let Some(control) = Control::of(effect) else {
            return false;
        };
        match control {
            Control::Toolbar(hit) => self.pending.set(Some(hit)),
            Control::Album(hit) => self.album_editor_pending.set(Some(hit)),
            Control::Details(hit) => self.metadata_editor_pending.set(Some(hit)),
            Control::Collection(hit) => self.collection_page_pending.set(Some(hit)),
            Control::Directory(hit) => self.directory_import_pending.set(Some(hit)),
            Control::People(intent) => *self.people_pending.borrow_mut() = Some(intent),
        }
        true
    }

    /// This surface editing through `dispatcher`: what a test of the
    /// surface's own methods attaches
    /// ([`testing::dispatcher`](ui::framework::testing::dispatcher)).
    #[must_use]
    pub fn with_dispatcher(
        mut self,
        dispatcher: ui::framework::Dispatcher<crate::model::PhotoLibrary>,
    ) -> Self {
        self.dispatch = Some(dispatcher);
        self
    }

    /// Submit a field edit or a verb to the library through its door. It
    /// is applied after this pass, which only reads the library; a refusal
    /// is reported by the dispatcher.
    fn submit(&self, submit: impl ui::framework::Submit<PhotoLibrary>) -> Result<(), String> {
        let Some(dispatch) = &self.dispatch else {
            return Err("this window edits no library".into());
        };
        if dispatch.read_only() {
            return Err("you can view this library but not change it".into());
        }
        dispatch.submit(submit);
        Ok(())
    }

    /// Every card interaction submits the same typed edits and verbs as
    /// resources, agents and search results do. The canvas never writes
    /// document fields directly.
    #[cfg_attr(
        not(test),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "a test records what it submitted"
        )
    )]
    fn commit(&mut self, submit: impl ui::framework::Submit<PhotoLibrary>) {
        let invoke = match submit.into_invoke() {
            Ok(invoke) => invoke,
            Err(error) => {
                tracing::warn!(%error, "photos: the library edit does not build");
                return;
            }
        };
        #[cfg(test)]
        {
            self.last_library_action = Some(invoke.clone());
        }
        if let Err(error) = self.submit(invoke) {
            tracing::warn!(%error, "photos: the library edit was refused");
        }
    }

    /// Submit `edits` as one change: one version, one undo step.
    fn commit_group(&mut self, edits: &[ui::tilo_resource::Edit<PhotoLibrary>]) {
        let members: Vec<_> = edits
            .iter()
            .filter_map(|edit| ui::tilo_resource::Invoke::edit(edit).ok())
            .collect();
        if !members.is_empty() {
            self.commit(ui::tilo_resource::Invoke::group(members));
        }
    }

    fn begin_new_album(&mut self) {
        // A local `untitled-1` counter collides when two offline replicas
        // create their first album. Mint the durable identity independently
        // of its editable name so concurrent creations remain separate.
        let id = format!("photos.album:{}", tilo_actions::InvocationId::new().0);
        self.album_editor = true;
        self.album_editor_creating = true;
        self.album_editor_id = Some(id);
        self.album_name_input.set_text(&tr("photos-untitled-album"));
        self.album_name_input.set_focus(true);
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-new-hint"));
    }

    /// Open the compact, shared-widget metadata sheet for an explicit card.
    /// Nothing is inferred from pixels: people are only labels the user types.
    fn begin_metadata_editor(&mut self, photo_id: &str) {
        self.begin_metadata_editor_for_ids(vec![photo_id.to_owned()]);
    }

    fn begin_selected_metadata_editor(&mut self) {
        let mut ids: Vec<_> = self.selected_photo_ids.iter().cloned().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            self.notice = Some(tr("photos-selection-empty"));
            return;
        }
        self.begin_metadata_editor_for_ids(ids);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "seeds every metadata field, mixed-value flag and draft input from one library snapshot"
    )]
    fn begin_metadata_editor_for_ids(&mut self, photo_ids: Vec<String>) {
        let library = self.presented();
        if photo_ids.is_empty()
            || photo_ids.len() > crate::resource::MAX_BATCH_PHOTOS
            || !photo_ids.iter().all(|id| library.photo(id).is_some())
        {
            self.notice = Some(tr("photos-error-selected-gone"));
            return;
        }
        let photos: Vec<_> = photo_ids
            .iter()
            .filter_map(|id| library.photo(id))
            .collect();
        let common = |values: Vec<String>| {
            values
                .first()
                .filter(|first| values.iter().all(|value| value == *first))
                .cloned()
        };
        let folders: Vec<_> = photos
            .iter()
            .map(|photo| photo.folder.clone().unwrap_or_default())
            .collect();
        let people: Vec<_> = photos
            .iter()
            .map(|photo| photo.people().join(", "))
            .collect();
        let tags: Vec<_> = photos.iter().map(|photo| photo.tags().join(", ")).collect();
        let captured: Vec<_> = photos
            .iter()
            .map(|photo| photo.captured_at.clone().unwrap_or_default())
            .collect();
        let locations: Vec<_> = photos.iter().map(|photo| photo.location.clone()).collect();
        let common_folder = common(folders);
        let common_people = common(people);
        let common_tags = common(tags);
        let common_captured = common(captured);
        let common_location = locations
            .first()
            .filter(|first| locations.iter().all(|value| value == *first))
            .cloned();

        self.metadata_editor = true;
        self.metadata_editor_ids = photo_ids;
        self.metadata_mixed = [
            common_folder.is_none(),
            common_people.is_none(),
            common_tags.is_none(),
            common_captured.is_none(),
            common_location.is_none(),
        ];
        self.metadata_force_clear = [false; 5];
        self.metadata_folder_input
            .set_text(common_folder.as_deref().unwrap_or_default());
        self.metadata_people_input
            .set_text(common_people.as_deref().unwrap_or_default());
        self.metadata_tags_input
            .set_text(common_tags.as_deref().unwrap_or_default());
        self.metadata_captured_at_input
            .set_text(common_captured.as_deref().unwrap_or_default());
        if let Some(Some(location)) = common_location {
            self.metadata_place_input
                .set_text(location.label.as_deref().unwrap_or_default());
            self.metadata_latitude_input
                .set_text(&format!("{:.6}", location.latitude));
            self.metadata_longitude_input
                .set_text(&format!("{:.6}", location.longitude));
            self.metadata_location_map.latitude = location.latitude;
            self.metadata_location_map.longitude = location.longitude;
            self.metadata_location_map.zoom = 4.0;
            self.metadata_location_from_map = location.provenance.source == "map";
        } else {
            self.metadata_place_input.set_text("");
            self.metadata_latitude_input.set_text("");
            self.metadata_longitude_input.set_text("");
            self.metadata_location_map = crate::places::MapViewport {
                zoom: crate::places::MIN_ZOOM,
                ..crate::places::MapViewport::default()
            };
            self.metadata_location_from_map = false;
        }
        // A field's placeholder says what to type, never its label again;
        // one that differs across the chosen photos says it is kept as is.
        let mixed_hint = tr("photos-metadata-mixed");
        for (input, mixed, ordinary) in [
            (
                &mut self.metadata_folder_input,
                self.metadata_mixed[0],
                String::new(),
            ),
            (
                &mut self.metadata_people_input,
                self.metadata_mixed[1],
                tr("photos-metadata-people-placeholder"),
            ),
            (
                &mut self.metadata_tags_input,
                self.metadata_mixed[2],
                tr("photos-metadata-tags-placeholder"),
            ),
            (
                &mut self.metadata_captured_at_input,
                self.metadata_mixed[3],
                tr("photos-metadata-captured-placeholder"),
            ),
            (
                &mut self.metadata_latitude_input,
                self.metadata_mixed[4],
                String::new(),
            ),
            (
                &mut self.metadata_longitude_input,
                self.metadata_mixed[4],
                String::new(),
            ),
        ] {
            input.set_placeholder(if mixed { &mixed_hint } else { &ordinary });
        }
        self.metadata_folder_input.set_focus(true);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_place_input.set_focus(false);
        self.metadata_latitude_input.set_focus(false);
        self.metadata_longitude_input.set_focus(false);
        self.metadata_editor_section = MetadataEditorSection::Organize;
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
        self.metadata_editor_ids.clear();
        self.metadata_folder_input.set_focus(false);
        self.metadata_people_input.set_focus(false);
        self.metadata_tags_input.set_focus(false);
        self.metadata_captured_at_input.set_focus(false);
        self.metadata_place_input.set_focus(false);
        self.metadata_latitude_input.set_focus(false);
        self.metadata_longitude_input.set_focus(false);
        self.metadata_editor_focus = 0;
        self.notice = Some(tr("photos-details-discarded"));
    }

    #[expect(
        clippy::too_many_lines,
        reason = "builds the single atomic batch metadata action from all sheet fields in one place"
    )]
    fn save_metadata_editor(&mut self) {
        if self.metadata_editor_ids.is_empty() {
            return;
        }
        let optional = |input: &TextInput| {
            let value = input.value().trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let text_patch = |input: &TextInput, mixed: bool, clear: bool| {
            if clear {
                crate::model::MetadataPatch::Clear
            } else if mixed && input.value().trim().is_empty() {
                crate::model::MetadataPatch::Keep
            } else {
                optional(input).map_or(
                    crate::model::MetadataPatch::Clear,
                    crate::model::MetadataPatch::Set,
                )
            }
        };
        let labels_patch = |input: &TextInput, mixed: bool, clear: bool| {
            if clear {
                crate::model::MetadataLabelsPatch::Clear
            } else {
                let raw = input.value().trim();
                let (mode, raw) = match raw.strip_prefix('=') {
                    Some(value) => ("set", value),
                    None => match raw.strip_prefix('+') {
                        Some(value) => ("add", value),
                        None => match raw.strip_prefix('-') {
                            Some(value) => ("remove", value),
                            None if self.metadata_editor_ids.len() > 1 && !raw.is_empty() => {
                                ("add", raw)
                            }
                            None => ("set", raw),
                        },
                    },
                };
                let values = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if mixed && values.is_empty() {
                    crate::model::MetadataLabelsPatch::Keep
                } else {
                    match mode {
                        "add" => crate::model::MetadataLabelsPatch::Add(values),
                        "remove" => crate::model::MetadataLabelsPatch::Remove(values),
                        _ if values.is_empty() => crate::model::MetadataLabelsPatch::Clear,
                        _ => crate::model::MetadataLabelsPatch::Set(values),
                    }
                }
            }
        };
        let latitude = self.metadata_latitude_input.value().trim();
        let longitude = self.metadata_longitude_input.value().trim();
        let location = if self.metadata_force_clear[4] {
            crate::model::MetadataPatch::Clear
        } else if self.metadata_mixed[4] && latitude.is_empty() && longitude.is_empty() {
            crate::model::MetadataPatch::Keep
        } else if latitude.is_empty() && longitude.is_empty() {
            crate::model::MetadataPatch::Clear
        } else {
            let (Ok(latitude), Ok(longitude)) = (latitude.parse::<f64>(), longitude.parse::<f64>())
            else {
                self.notice = Some(tr("photos-location-invalid"));
                return;
            };
            crate::model::MetadataPatch::Set(crate::model::PhotoLocation {
                latitude,
                longitude,
                label: optional(&self.metadata_place_input),
                provenance: crate::model::LocationProvenance {
                    source: if self.metadata_location_from_map {
                        "map".to_owned()
                    } else {
                        "user".to_owned()
                    },
                    scope: "shared".to_owned(),
                },
            })
        };
        let batch = crate::model::MetadataBatch {
            folder: text_patch(
                &self.metadata_folder_input,
                self.metadata_mixed[0],
                self.metadata_force_clear[0],
            ),
            people: labels_patch(
                &self.metadata_people_input,
                self.metadata_mixed[1],
                self.metadata_force_clear[1],
            ),
            tags: labels_patch(
                &self.metadata_tags_input,
                self.metadata_mixed[2],
                self.metadata_force_clear[2],
            ),
            captured_at: text_patch(
                &self.metadata_captured_at_input,
                self.metadata_mixed[3],
                self.metadata_force_clear[3],
            ),
            location,
        };
        match self
            .presented()
            .metadata_edits(&self.metadata_editor_ids, &batch)
        {
            Ok(edits) => self.commit_group(&edits),
            Err(refusal) => {
                self.notice = Some(refusal.0);
                return;
            }
        }
        let count = self.metadata_editor_ids.len();
        self.metadata_editor = false;
        self.metadata_editor_ids.clear();
        self.metadata_editor_focus = 0;
        self.notice = Some(if count == 1 {
            tr("photos-details-saved")
        } else {
            tr_count("photos-details-saved-many", count)
        });
        if count > 1 {
            self.finish_selection();
        }
    }

    fn set_metadata_editor_focus(&mut self, focus: usize) {
        let max = if self.metadata_editor_section == MetadataEditorSection::Organize {
            4
        } else {
            5
        };
        self.metadata_editor_focus = focus.min(max);
        self.metadata_editor_reveal_focus = true;
        let organize = self.metadata_editor_section == MetadataEditorSection::Organize;
        self.metadata_folder_input.set_focus(organize && focus == 0);
        self.metadata_people_input.set_focus(organize && focus == 1);
        self.metadata_tags_input.set_focus(organize && focus == 2);
        self.metadata_captured_at_input
            .set_focus(!organize && focus == 0);
        self.metadata_place_input.set_focus(!organize && focus == 1);
        self.metadata_latitude_input
            .set_focus(!organize && focus == 2);
        self.metadata_longitude_input
            .set_focus(!organize && focus == 3);
    }

    fn set_metadata_editor_section(&mut self, section: MetadataEditorSection) {
        self.metadata_editor_section = section;
        self.metadata_editor_scroll = 0.0;
        self.set_metadata_editor_focus(0);
    }

    fn clear_metadata_dimension(&mut self, dimension: usize) {
        let Some(force_clear) = self.metadata_force_clear.get_mut(dimension) else {
            return;
        };
        *force_clear = !*force_clear;
        if !*force_clear {
            return;
        }
        match dimension {
            0 => self.metadata_folder_input.set_text(""),
            1 => self.metadata_people_input.set_text(""),
            2 => self.metadata_tags_input.set_text(""),
            3 => self.metadata_captured_at_input.set_text(""),
            4 => {
                self.metadata_place_input.set_text("");
                self.metadata_latitude_input.set_text("");
                self.metadata_longitude_input.set_text("");
                self.metadata_location_from_map = false;
            }
            _ => {}
        }
    }

    /// Write an explicit offline-map point into the draft fields. No document
    /// update occurs here: batch edits retain their mixed values until the
    /// user deliberately saves this one shared provenance-bearing patch.
    fn set_metadata_location_from_map_point(&mut self, point: Point) -> bool {
        let bounds = self.metadata_location_map_bounds;
        if !bounds.contains(point) {
            return false;
        }
        let Some((latitude, longitude)) = self.metadata_location_map.unproject(
            point.x - bounds.x0,
            point.y - bounds.y0,
            bounds.width(),
            bounds.height(),
        ) else {
            return false;
        };
        self.metadata_latitude_input
            .set_text(&format!("{latitude:.6}"));
        self.metadata_longitude_input
            .set_text(&format!("{longitude:.6}"));
        // Re-centre the private draft map so its persistent crosshair is an
        // honest confirmation of the point just chosen.
        self.metadata_location_map.latitude = latitude;
        self.metadata_location_map.longitude = longitude;
        self.metadata_mixed[4] = false;
        self.metadata_force_clear[4] = false;
        self.metadata_location_from_map = true;
        self.notice = Some(tr("photos-location-picked"));
        true
    }

    fn activate_metadata_editor_hit(&mut self, hit: MetadataEditorHit) {
        match hit {
            MetadataEditorHit::Save => self.save_metadata_editor(),
            MetadataEditorHit::Cancel => self.cancel_metadata_editor(),
            MetadataEditorHit::Organize => {
                self.set_metadata_editor_section(MetadataEditorSection::Organize);
            }
            MetadataEditorHit::TimeAndPlace => {
                self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
            }
            MetadataEditorHit::ClearFolder => self.clear_metadata_dimension(0),
            MetadataEditorHit::ClearPeople => self.clear_metadata_dimension(1),
            MetadataEditorHit::ClearTags => self.clear_metadata_dimension(2),
            MetadataEditorHit::ClearCapturedAt => self.clear_metadata_dimension(3),
            MetadataEditorHit::ClearLocation => self.clear_metadata_dimension(4),
            MetadataEditorHit::PlaceOnMap => {
                let bounds = self.metadata_location_map_bounds;
                let _ = self.set_metadata_location_from_map_point(bounds.center());
            }
        }
    }

    fn begin_edit_album(&mut self) {
        let Some(id) = self.active_album.clone() else {
            self.notice = Some(tr("photos-album-choose"));
            return;
        };
        let Some(album) = self.presented().album(&id).cloned() else {
            self.notice = Some("That album is no longer available".to_owned());
            self.active_album = None;
            return;
        };
        self.album_editor = true;
        self.album_editor_creating = false;
        self.album_editor_id = Some(album.id);
        self.album_name_input.set_text(&album.name);
        self.album_name_input.set_focus(true);
        self.album_editor_members = album.photo_ids.iter().cloned().collect();
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
        self.notice = Some(tr("photos-album-discarded"));
    }

    fn save_album_editor(&mut self) {
        let Some(id) = self.album_editor_id.clone() else {
            return;
        };
        let name = self.album_name_input.value().trim().to_owned();
        let library = self.presented();
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
                tr("photos-album-conflict")
            } else {
                tr("photos-album-invalid")
            });
            return;
        }
        let photo_ids: ui::tilo_resource::Set<String> = library
            .all_photos()
            .filter(|photo| self.album_editor_members.contains(&photo.id))
            .map(|photo| photo.id.clone())
            .collect();
        let album = crate::model::Album {
            id: id.clone(),
            name,
            photo_ids,
        };
        if self.album_editor_creating {
            self.commit(
                PhotoLibrary::paths()
                    .albums
                    .insert_as(&album.item(), usize::MAX, &album),
            );
        } else {
            // A name and a membership saved together, so collaborators never
            // see half of the save.
            let paths = crate::model::album_paths(&id);
            let before = library
                .album(&id)
                .map(|album| album.photo_ids.clone())
                .unwrap_or_default();
            let mut edits = vec![paths.name.replace(album.name)];
            edits.extend(paths.photo_ids.edits(&before, &album.photo_ids));
            self.commit_group(&edits);
        }
        self.active_album = Some(id);
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-saved"));
    }

    fn delete_active_album(&mut self) {
        let Some(id) = self
            .album_editor_id
            .clone()
            .or_else(|| self.active_album.clone())
        else {
            return;
        };
        self.commit(PhotoLibrary::REMOVE_ALBUM.with(Id::from_raw(id)));
        self.active_album = None;
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some(tr("photos-album-deleted"));
    }

    /// Move through the bounded library shelf without changing any shared
    /// state.  Album editing keeps its unsaved membership locally, so paging
    /// must stay available while the sheet is open: otherwise a phone-sized
    /// sheet can leave most of a large library impossible to include.
    fn library_page_step(&self) -> usize {
        if self.layout_theme.text_scale > 1.25 && self.canvas_rect.width() < 620.0 {
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
        let viewport = self.timeline_viewport_height.max(1.0);
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + if forward { viewport } else { -viewport },
            viewport,
        );
    }

    fn can_page_library(&self, forward: bool) -> bool {
        let step = self.library_page_step();
        if forward {
            self.library_offset.saturating_add(step) < self.library_total
        } else {
            self.library_offset > 0
        }
    }

    fn page_collection(&mut self, hit: CollectionPageHit) -> bool {
        let place_total = self
            .places_items
            .iter()
            .filter(|item| {
                self.places_open_key.as_ref().is_none_or(|key| {
                    item.photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == *key)
                })
            })
            .count();
        let page_size = COLLECTION_PAGE_SIZE;
        let (page, total, forward) = match hit {
            CollectionPageHit::AlbumsPrevious => {
                (&mut self.albums_page, self.library_albums.len(), false)
            }
            CollectionPageHit::AlbumsNext => {
                (&mut self.albums_page, self.library_albums.len(), true)
            }
            CollectionPageHit::PeoplePrevious => {
                (&mut self.people_page, self.library_people.len(), false)
            }
            CollectionPageHit::PeopleNext => {
                (&mut self.people_page, self.library_people.len(), true)
            }
            CollectionPageHit::PlacesPrevious => (&mut self.places_page, place_total, false),
            CollectionPageHit::PlacesNext => (&mut self.places_page, place_total, true),
        };
        let pages = total.max(1).div_ceil(page_size);
        let next = if forward {
            page.saturating_add(1).min(pages.saturating_sub(1))
        } else {
            page.saturating_sub(1)
        };
        if next == *page {
            return false;
        }
        *page = next;
        if matches!(
            hit,
            CollectionPageHit::AlbumsPrevious | CollectionPageHit::AlbumsNext
        ) {
            let step = self.albums_viewport.height();
            if step > 0.0 {
                self.albums_scroll =
                    (self.albums_scroll + if forward { step } else { -step }).max(0.0);
            }
        }
        true
    }

    fn library_page_label(&self) -> String {
        let first = (self.library_offset + 1).min(self.library_total);
        let last = (self.library_offset + self.library_page_step()).min(self.library_total);
        tr_args(
            "photos-timeline-showing",
            &[
                ("first", ArgValue::Number(first as f64)),
                ("last", ArgValue::Number(last as f64)),
                ("total", ArgValue::Number(self.library_total as f64)),
            ],
        )
    }

    /// Change the local timeline density through any input source.  One step
    /// is shared by the +/- affordances, Ctrl-wheel and pinch, which makes
    /// their resulting layout testable for parity.
    fn adjust_timeline_density(&mut self, increase: bool) -> bool {
        let before = self.timeline_density;
        if increase {
            self.timeline_density = self.timeline_density.saturating_add(1).min(4);
        } else {
            self.timeline_density = self.timeline_density.saturating_sub(1);
        }
        if before == self.timeline_density {
            return false;
        }
        true
    }

    fn scroll_timeline_rows(&mut self, delta: f64) {
        // Keyboard rows remain a useful semantic step, but the canonical
        // offset is pixels so portrait and justified rows scroll smoothly.
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + delta * 168.0,
            self.timeline_viewport_height,
        );
    }

    fn scroll_timeline_pixels(&mut self, delta: f64) {
        if !delta.is_finite() {
            return;
        }
        self.timeline_scroll_y = self.timeline_projection.clamp_scroll(
            self.timeline_scroll_y + delta,
            self.timeline_viewport_height,
        );
    }

    /// Apply every chronological-index input through the same local scroll
    /// and density helpers used by the ordinary timeline.  The index never
    /// owns a second offset, which is what keeps a pinch anchor and a dragged
    /// date thumb in agreement.
    fn apply_timeline_index_intent(&mut self, intent: &DateIndexIntent) -> bool {
        match *intent {
            DateIndexIntent::ScrollTo { position } => {
                self.timeline_scroll_y =
                    DateIndexedScrollbar::offset_for(self.timeline_index_metrics, position);
                true
            }
            DateIndexIntent::AdjustDensity { delta } => match delta.cmp(&0) {
                std::cmp::Ordering::Greater => self.adjust_timeline_density(true),
                std::cmp::Ordering::Less => self.adjust_timeline_density(false),
                std::cmp::Ordering::Equal => false,
            },
            // A preview is presentation only. The widget has retained it for
            // paint and AT's current value; it must not make collaboration
            // history noisy by changing a collection just because it was
            // hovered.
            DateIndexIntent::PreviewChanged(_) => true,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "embedded TextInput uses the Widget event contract; keyboard and pointer handling stay beside the album-sheet controls"
    )]
    fn handle_album_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.album_editor {
            return false;
        }
        // The timeline beside the editor keeps scrolling: choosing photos
        // means finding them.
        if matches!(event, winit::event::WindowEvent::MouseWheel { .. }) {
            return false;
        }
        let mut sink = None;
        self.album_name_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                focus_target: &mut sink,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::PageDown) => {
                    self.scroll_timeline_rows(3.0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::PageUp) => {
                    self.scroll_timeline_rows(-3.0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Tab) => {
                    let steps = if self.album_editor_creating { 3 } else { 4 };
                    self.album_editor_focus = if ctx.modifiers.state().shift_key() {
                        (self.album_editor_focus + steps - 1) % steps
                    } else {
                        (self.album_editor_focus + 1) % steps
                    };
                    self.album_name_input
                        .set_focus(self.album_editor_focus == 0);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_album_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() =>
                {
                    self.save_album_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    match (self.album_editor_creating, self.album_editor_focus) {
                        (_, 1) => self.cancel_album_editor(),
                        (false, 2) if self.album_delete_confirm => {
                            self.delete_active_album();
                        }
                        (false, 2) => self.album_delete_confirm = true,
                        (true, 2) | (false, 3) => self.save_album_editor(),
                        _ => {}
                    }
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
            if self.metadata_editor_section == MetadataEditorSection::TimeAndPlace
                && matches!(self.metadata_editor_focus, 2 | 3)
            {
                // Coordinates subsequently authored through the shared text
                // input are no longer accurately described as a map pin.
                self.metadata_location_from_map = false;
            }
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) && self.metadata_editor_section == MetadataEditorSection::TimeAndPlace
            && self.set_metadata_location_from_map_point(self.pointer)
        {
            *ctx.handled = true;
            return true;
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
                    AlbumEditorHit::Save => self.save_album_editor(),
                    AlbumEditorHit::Cancel => self.cancel_album_editor(),
                    AlbumEditorHit::Delete => {
                        self.album_delete_confirm = true;
                        self.notice = Some(tr("photos-album-delete-confirm"));
                    }
                    AlbumEditorHit::ConfirmDelete => self.delete_active_album(),
                    _ => {}
                }
                *ctx.handled = true;
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
                *ctx.handled = true;
                return true;
            }
        }
        true
    }

    fn handle_collection_page_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.library_mode || self.collection_page_hits.is_empty() {
            return false;
        }
        for (hit, button) in &mut self.collection_page_buttons {
            if self
                .collection_page_hits
                .iter()
                .any(|(visible, _, _)| visible == hit)
            {
                let mut sink = None;
                button.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut sink,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
            }
        }
        if let Some(hit) = self.collection_page_pending.take() {
            self.page_collection(hit);
            *ctx.handled = true;
            return true;
        }
        false
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keyboard, pointer and embedded TextInput routing for the metadata sheet stay together under the Widget event contract"
    )]
    fn handle_metadata_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.metadata_editor {
            return false;
        }
        // The sheet floats over a custom media surface, so dispatch its
        // retained controls explicitly. They still use the normal widget
        // event contract and publish the same action surface as every other
        // button in the app.
        for (_, button) in &mut self.metadata_editor_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit);
            *ctx.handled = true;
            return true;
        }
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            let focus_count = if self.metadata_editor_section == MetadataEditorSection::Organize {
                5
            } else {
                6
            };
            match &key.logical_key {
                Key::Named(NamedKey::Tab) => {
                    self.set_metadata_editor_focus(if ctx.modifiers.state().shift_key() {
                        (self.metadata_editor_focus + focus_count - 1) % focus_count
                    } else {
                        (self.metadata_editor_focus + 1) % focus_count
                    });
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() =>
                {
                    self.save_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == focus_count - 2 => {
                    self.cancel_metadata_editor();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.metadata_editor_focus == focus_count - 1 => {
                    self.save_metadata_editor();
                    *ctx.handled = true;
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
                winit::event::MouseScrollDelta::PixelDelta(position) => {
                    position.y / ctx.scale_factor
                }
            };
            self.metadata_editor_scroll = (self.metadata_editor_scroll - dy).max(0.0);
            *ctx.handled = true;
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
            &mut self.metadata_place_input,
            &mut self.metadata_latitude_input,
            &mut self.metadata_longitude_input,
        ] {
            if press_outside_body {
                break;
            }
            let mut sink = None;
            input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        // A field's clear toggle answers in the same event.
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit);
            *ctx.handled = true;
            return true;
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
            self.activate_metadata_editor_hit(hit);
            *ctx.handled = true;
        }
        true
    }

    /// What shows the photograph or album a search hit or an agent's `open`
    /// names, for [`ViewCx::on_open_item`](ui::framework::ViewCx::on_open_item).
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&Id) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let opens = Rc::clone(&self.opens);
        move |id| {
            if !id.as_str().starts_with("photos.photo:")
                && !id.as_str().starts_with("photos.album:")
            {
                return Err(ui::tilo_resource::Refusal::new(
                    "only a photograph or an album opens",
                ));
            }
            opens.borrow_mut().push(id.clone());
            Ok(())
        }
    }

    /// Show what was asked to be opened since the last sync.
    fn drain_opens(&mut self) {
        let opens = std::mem::take(&mut *self.opens.borrow_mut());
        for id in opens {
            let library = self.presented();
            let effect = if id.as_str().starts_with("photos.album:") {
                PhotosEffect::ViewAlbum {
                    album_id: id.as_str().to_owned(),
                }
            } else {
                PhotosEffect::View {
                    photo_id: id.as_str().to_owned(),
                }
            };
            if let Err(error) = self.perform_effect(effect, &library) {
                self.notice = Some(error);
            }
        }
    }

    /// Perform a device-local operation and surface failures to canonical
    /// operation dispatch. A local notice alone is not an invocation result.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over device-local Photos operations keeps their effects auditable"
    )]
    fn perform_effect(
        &mut self,
        operation: PhotosEffect,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        match operation {
            // Resource/search routing has already brought this Photos
            // instance forward. Viewing does not alter document or OS state.
            PhotosEffect::Import { path: None } => {
                self.import_content = Some(LiveContent {
                    library: library.clone(),
                });
                self.picker_stale = false;
                let filter = ui::portal::FileFilter::new(&tr("photos-filter-images"))
                    .mimetype("image/png")
                    .mimetype("image/jpeg")
                    .mimetype("image/webp");
                if crate::files::native_paths() {
                    self.picker = Some(
                        ui::portal::PickRequest::open(&tr("photos-open-title"), &tr("photos-open"))
                            .thread_name("photos-portal-open")
                            .filter(filter)
                            .all_files()
                            .send(),
                    );
                } else {
                    self.contents_picker = Some(
                        ui::portal::PickRequest::open_multiple(
                            &tr("photos-open-title"),
                            &tr("photos-open"),
                        )
                        .filter(filter)
                        .send_contents(),
                    );
                }
            }
            PhotosEffect::ImportDirectory {
                path: None,
                retain_exif_location,
            } => {
                self.directory_picker_retain_exif_location = retain_exif_location.unwrap_or(true);
                self.directory_picker = Some(
                    ui::portal::PickRequest::directory(
                        &tr("photos-import-pick-title"),
                        &tr("photos-import-pick-accept"),
                    )
                    .thread_name("photos-portal-directory-import")
                    .send(),
                );
            }
            PhotosEffect::ImportDirectory {
                path: Some(path),
                retain_exif_location,
            } => {
                self.begin_directory_import(
                    std::path::PathBuf::from(path),
                    None,
                    DirectoryImportMode::Import,
                    retain_exif_location.unwrap_or(true),
                );
            }
            PhotosEffect::ImportCameraMedia { media_id, name } => {
                return Err(format!(
                    "Camera import requires document dispatch: {media_id} ({name})"
                ));
            }
            PhotosEffect::ReindexDirectory {
                path,
                retain_exif_location,
            } => {
                self.begin_directory_import(
                    std::path::PathBuf::from(path),
                    None,
                    DirectoryImportMode::Reindex,
                    retain_exif_location.unwrap_or(true),
                );
            }
            PhotosEffect::AnalyzeText { .. }
            | PhotosEffect::AnalyzeImage { .. }
            | PhotosEffect::AnalyzePeopleLibrary
            | PhotosEffect::AnalyzeLibrary
            | PhotosEffect::PauseLibraryAnalysis
            | PhotosEffect::ResumeLibraryAnalysis
            | PhotosEffect::RetryLibraryAnalysisFailures => {
                return Err("local image analysis requires document dispatch".to_owned());
            }
            PhotosEffect::Copy => {
                self.copy_to_clipboard();
            }
            PhotosEffect::ShareReference { resource_id } => {
                let exists = library.photo(&resource_id).cloned().is_some();
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
            PhotosEffect::Save { path } => {
                if let Some(open) = self.shown_photo_in(library) {
                    self.save_to(
                        path.as_deref()
                            .unwrap_or(&Self::suggested_save_path(&open.image)),
                    );
                }
            }
            PhotosEffect::ExportEditedContent { .. } => {
                return Err("edited content export requires lifecycle dispatch".into());
            }
            PhotosEffect::View { photo_id } => {
                self.view_photo(library, &photo_id)?;
            }
            PhotosEffect::ViewLibrary => self.show_library(),
            PhotosEffect::ViewAlbums => self.show_albums(),
            PhotosEffect::ViewTimeline { anchor } => {
                self.view_timeline(library, anchor.as_ref())?;
            }
            PhotosEffect::ViewPeople => {
                self.show_library();
                self.people_mode = true;
                self.library_destination = LibraryDestination::People;
            }
            PhotosEffect::ViewLibraryAnalysis => {
                self.show_library_analysis();
            }
            PhotosEffect::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Models in Settings to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            PhotosEffect::EnablePeopleRecognition => {
                self.people_controller.set_recognition_enabled(true);
                self.people_controller
                    .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                self.notice = Some(self.people_controller.capability().summary());
            }
            PhotosEffect::ViewFavorites => self.view_favorites(library)?,
            PhotosEffect::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.places_open_key = None;
                self.places_fit_pending = true;
            }
            PhotosEffect::ViewPlace { place_key } => {
                self.open_place_collection(library, &place_key)?;
            }
            // Views with a library to check, a direct import and the
            // retained controls' effects are answered before this.
            _ => {}
        }
        Ok(())
    }

    /// Navigate to an extant stable album id without producing a CRDT action.
    /// This is deliberately separate from `perform_effect`: callers need a
    /// useful stale-address error instead of silently falling back to all
    /// photographs if a collaborator removed the album after search indexed
    /// it.
    /// Open photograph `photo_id` in this window, from search, the palette
    /// or an agent.
    fn view_photo(
        &mut self,
        library: &crate::model::PhotoLibrary,
        photo_id: &str,
    ) -> Result<(), String> {
        if library.photo(photo_id).cloned().is_none() {
            return Err("that photograph is no longer in the library".to_owned());
        }
        self.show_photo(photo_id);
        self.album_editor = false;
        self.metadata_editor = false;
        Ok(())
    }

    fn view_album(
        &mut self,
        library: &crate::model::PhotoLibrary,
        album_id: &str,
    ) -> Result<(), String> {
        if library.album(album_id).cloned().is_none() {
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
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        Ok(())
    }

    /// Enter a metadata-derived library view after verifying the label still
    /// exists. An old global-search result therefore fails honestly when a
    /// collaborator removes the final matching photograph.
    fn view_metadata_filter(
        &mut self,
        library: &crate::model::PhotoLibrary,
        folder: Option<String>,
        person: Option<String>,
        tag: Option<String>,
    ) -> Result<(), String> {
        let exists = match (&folder, &person, &tag) {
            (Some(folder), None, None) => library
                .photos
                .values()
                .any(|photo| photo.folder.as_deref() == Some(folder)),
            (None, Some(person), None) => library
                .photos
                .values()
                .any(|photo| photo.people.iter().any(|value| value == person)),
            (None, None, Some(tag)) => library
                .photos
                .values()
                .any(|photo| photo.tags.iter().any(|value| value == tag)),
            _ => false,
        };
        if !exists {
            return Err(tr("photos-error-filter-gone"));
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
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        Ok(())
    }

    /// Open one exact intelligence-derived collection. These labels are
    /// still local navigation; only the accepted analysis record is shared.
    fn view_analysis_filter(
        &mut self,
        library: &crate::model::PhotoLibrary,
        label: String,
        pet: bool,
    ) -> Result<(), String> {
        let exists = library.photos.values().any(|photo| {
            photo.analysis().is_some_and(|analysis| {
                if pet {
                    analysis.pets.contains(&label)
                } else {
                    analysis.objects.contains(&label)
                }
            })
        });
        if !exists {
            return Err(tr("photos-error-filter-gone"));
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
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        Ok(())
    }

    /// Navigate to the stable favorites resource after verifying that it is
    /// still non-empty. A stale global-search result must fail rather than
    /// silently changing the user's view to an unrelated all-photos shelf.
    fn view_favorites(&mut self, library: &crate::model::PhotoLibrary) -> Result<(), String> {
        if !library.photos.values().any(|photo| photo.favorite) {
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
        self.timeline_scroll_y = 0.0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        Ok(())
    }

    /// Project the open photograph's portable library record into the small
    /// viewer card. Content identity wins; old path-only records retain their
    /// legacy match so opening one still has useful context.
    fn viewer_details_for(
        library: &crate::model::PhotoLibrary,
        open: &OpenImage,
        edits: Edits,
    ) -> ViewerDetails {
        let photo = library
            .photos
            .values()
            .find(|photo| match (&open.content, &photo.image.content) {
                (Some(open_content), Some(photo_content)) => open_content == photo_content,
                (None, None) => open.path == photo.image.path,
                _ => false,
            })
            .cloned();
        let _ = edits;
        ViewerDetails {
            name: open.name.clone(),
            description: photo
                .as_ref()
                .and_then(|photo| photo.vision.as_ref())
                .map(|vision| vision.description.trim())
                .filter(|description| !description.is_empty())
                .map(str::to_owned),
            captured_at: photo.as_ref().and_then(|photo| photo.captured_at.clone()),
            place: photo.as_ref().and_then(|photo| {
                photo
                    .location
                    .as_ref()
                    .filter(|location| location.is_valid())
                    .map(crate::model::PhotoLocation::display_name)
            }),
            people: photo.as_ref().map_or_else(Vec::new, LibraryPhoto::people),
            tags: photo.as_ref().map_or_else(Vec::new, LibraryPhoto::tags),
            folder: photo.and_then(|photo| photo.folder),
        }
    }

    /// Re-derive the rendering if the path or the edits have moved.
    fn refresh(&mut self) {
        let Some((open, edits)) = self.shown_photo().map(|photo| (photo.image, photo.edits)) else {
            self.rendered = None;
            self.close_viewer();
            self.viewer_details = None;
            self.original = None;
            self.canvas_state = if self.notice.is_some() {
                CanvasState::Error
            } else {
                CanvasState::Empty
            };
            return;
        };
        self.viewer_details = Some(Self::viewer_details_for(&self.presented(), &open, edits));

        // Load the original if this is a picture we have not decoded yet. A
        // peer may have opened it, so this is not only the local Open path.
        let identity = decode_key(&open);
        if self.original_identity.as_ref() != Some(&identity) || self.original.is_none() {
            let key = decode_key(&open);
            if self.original_decode_failed.as_deref() == Some(&key) {
                self.original = None;
                self.rendered = None;
                self.close_viewer();
                self.canvas_state = CanvasState::Error;
                return;
            }
            let bytes = crate::model::bytes_of(&open, &self.content);
            if let Some(img) = bytes.as_deref().and_then(pixels::decode) {
                self.original = Some((open.path, img));
                self.original_identity = Some(identity.clone());
                self.original_decode_failed = None;
            } else {
                self.original = None;
                self.rendered = None;
                self.close_viewer();
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
        self.structure_dirty = true;
        self.canvas_state = CanvasState::Ready;
        self.rendered = Some(Rendered {
            identity,
            edits,
            rgba,
        });
    }

    /// Drain a click that a button posted, and act on it.
    fn drain_pending(&mut self) {
        let Some(hit) = self.pending.take() else {
            return;
        };
        let edits = self
            .shown_photo()
            .map(|photo| photo.edits)
            .unwrap_or_default();
        match hit {
            Hit::Open => {
                let _ = self.perform_effect(PhotosEffect::Import { path: None }, &self.presented());
            }
            Hit::Library if self.library_mode && self.people_selection.is_some() => {
                self.handle_people_intent(PeopleIntent::Back);
            }
            Hit::Library => {
                let _ = self.perform_effect(PhotosEffect::ViewLibrary, &self.presented());
            }
            Hit::RotateLeft => self.edit_shown(|photo| PhotoLibrary::ROTATE_LEFT.with(photo)),
            Hit::RotateRight => self.edit_shown(|photo| PhotoLibrary::ROTATE_RIGHT.with(photo)),
            Hit::CropToView => {
                let view = self.visible_rect();
                // Cropping to the whole image is a no-op; say so rather than
                // writing an edit that changes nothing.
                if view.is_full() {
                    self.notice = Some(tr("photos-crop-noop"));
                } else {
                    self.edit_shown(|photo| PhotoLibrary::CROP.with((photo, view)));
                }
            }
            Hit::Reset if !edits.is_identity() => self.edit_shown(|photo| {
                let paths = crate::model::photo_paths(photo.as_str()).edits;
                ui::tilo_resource::Invoke::group(
                    [paths.crop.set(Rect01::FULL), paths.rotate_quarters.set(0)]
                        .iter()
                        .filter_map(|edit| ui::tilo_resource::Invoke::edit(edit).ok()),
                )
            }),
            Hit::Reset => {}
            Hit::Copy => {
                let _ = self.perform_effect(PhotosEffect::Copy, &self.presented());
            }
            Hit::Share => {
                if let Some(photo_id) = self.open_photo.clone() {
                    let _ = self.perform_effect(
                        PhotosEffect::ShareReference {
                            resource_id: photo_id,
                        },
                        &self.presented(),
                    );
                } else {
                    self.notice = Some(tr("photos-share-failed"));
                }
            }
            Hit::Save => {
                let _ = self.perform_effect(PhotosEffect::Save { path: None }, &self.presented());
            }
            Hit::Select => {
                if self.selection_mode {
                    self.finish_selection();
                } else {
                    self.begin_selection();
                }
            }
            Hit::PreviousPage => self.page_library(false),
            Hit::NextPage => self.page_library(true),
            Hit::NewAlbum => self.begin_new_album(),
            Hit::EditAlbum => self.begin_edit_album(),
            Hit::Smaller => {
                self.adjust_timeline_density(true);
            }
            Hit::Larger => {
                self.adjust_timeline_density(false);
            }
            Hit::PeopleHidden => self.handle_people_intent(PeopleIntent::ToggleHidden),
            Hit::PlacesList => self.places_list_mode = !self.places_list_mode,
            Hit::PlacesZoomIn => self.zoom_places_by(1.0),
            Hit::PlacesZoomOut => self.zoom_places_by(-1.0),
            Hit::Selection(hit) => self.handle_selection_hit(hit),
            Hit::More => {
                if self.chrome.toolbar.shown_menu().is_some() {
                    self.chrome.toolbar.close_menus();
                } else {
                    self.chrome.toolbar.open_overflow();
                }
            }
            Hit::Search => self
                .chrome
                .search_opener
                .open(ui::search::SearchRequest::this_app()),
        }
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
        let (image_name, mut image_description) = match self.rendered.as_ref() {
            Some(rendered) if rendered.edits.is_identity() => {
                (image_label(), tr("photos-image-help"))
            }
            Some(_) => (
                format!("{} — {}", image_label(), tr("photos-edited")),
                tr("photos-image-help"),
            ),
            None => self.empty_state_text(),
        };
        if let Some(details) = self.viewer_details.as_ref() {
            let mut shared = Vec::new();
            if let Some(description) = details.description.as_deref() {
                shared.push(format!("{}: {description}", tr("photos-description")));
            }
            if let Some(captured_at) = details.captured_at.as_deref() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-captured"),
                    crate::model::capture_label(captured_at)
                ));
            }
            if let Some(place) = details.place.as_deref() {
                shared.push(format!("{}: {place}", tr("photos-place")));
            }
            if !details.people.is_empty() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-people"),
                    details.people.join(", ")
                ));
            }
            if !details.tags.is_empty() {
                shared.push(format!(
                    "{}: {}",
                    tr("photos-tags"),
                    details.tags.join(", ")
                ));
            }
            if let Some(folder) = details.folder.as_deref() {
                shared.push(format!("{}: {folder}", tr("photos-folder")));
            }
            if !shared.is_empty() {
                image_description.push_str(". ");
                image_description.push_str(&shared.join(". "));
            }
        }
        let canvas = if self.library_mode {
            let mut cards = Vec::new();
            if self.library_destination == LibraryDestination::Analysis {
                cards.extend(
                    self.library_analysis_buttons
                        .iter()
                        .filter_map(|(hit, button)| {
                            let bounds = button.get_rect();
                            if bounds == Rect::ZERO {
                                return None;
                            }
                            let (name, target) = match hit {
                                LibraryAnalysisHit::Start => {
                                    ("photos-a11y-analyze", "photos:analysis:start")
                                }
                                LibraryAnalysisHit::Pause => {
                                    ("photos-analysis-pause", "photos:analysis:pause")
                                }
                                LibraryAnalysisHit::Resume => {
                                    ("photos-a11y-resume-analysis", "photos:analysis:resume")
                                }
                                LibraryAnalysisHit::RetryFailures => {
                                    ("photos-analysis-retry", "photos:analysis:retry")
                                }
                            };
                            Some(
                                AccessNode::new(Role::Button)
                                    .with_name(tr(name))
                                    .with_bounds(bounds)
                                    .with_state(AccessState {
                                        focusable: button.is_focusable(),
                                        focused: button.is_focused(),
                                        disabled: !button.is_enabled(),
                                        ..Default::default()
                                    })
                                    .with_action_target(target),
                            )
                        }),
                );
                cards.extend(
                    self.library_analysis_failure_hits
                        .iter()
                        .map(|(id, bounds)| {
                            let failure = self
                                .library_analysis
                                .failures()
                                .find(|failure| failure.photo_id == *id);
                            AccessNode::new(Role::Button)
                                .with_name(failure.map_or_else(
                                    || tr("photos-a11y-open-failed"),
                                    |failure| {
                                        tr_with("photos-a11y-open-named", "name", &failure.name)
                                    },
                                ))
                                .with_description(failure.map_or_else(
                                    || tr("photos-a11y-analysis-failed"),
                                    |failure| failure.message.clone(),
                                ))
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(format!("photos:analysis:failure:{id}"))
                                .with_hashed_key(format!("photos:analysis-failure:{id}"))
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-analysis-label"))
                    .with_description(tr("photos-a11y-analysis-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:library-analysis")
                    .with_live(Politeness::Polite)
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::Albums {
                cards.extend(self.album_cards.iter().filter_map(|(id, bounds)| {
                    let id = id.as_ref()?;
                    let (_, name, count) = self
                        .library_albums
                        .iter()
                        .find(|(album_id, _, _)| album_id == id)?;
                    Some(
                        AccessNode::new(Role::ListItem)
                            .with_name(tr_args(
                                "photos-people-card-name",
                                &[
                                    ("name", ArgValue::Text(name.clone())),
                                    ("count", ArgValue::Number(*count as f64)),
                                ],
                            ))
                            .with_description(tr("photos-a11y-open-album"))
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_hashed_key(format!("photos:album-card:{id}"))
                            .with_action_target(format!("photos:library-album:{id}")),
                    )
                }));
                cards.extend(self.album_edit_cards.iter().map(|(id, bounds)| {
                    AccessNode::new(Role::Button)
                        .with_name(tr("photos-edit-album"))
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                        .with_hashed_key(format!("photos:album-edit:{id}"))
                        .with_action_target(format!("photos:album-edit:{id}"))
                }));
                AccessNode::new(Role::List)
                    .with_name(tr("photos-albums"))
                    .with_description(tr("photos-a11y-albums-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:albums")
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::People || self.people_mode {
                if self.people_selection.is_some() && !self.people_merge_mode {
                    cards.push(
                        AccessNode::new(Role::TextInput)
                            .with_name(tr("photos-a11y-person-name"))
                            .with_value(self.people_name_input.value())
                            .with_bounds(self.people_name_input.get_rect())
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.people_name_input.is_focused(),
                                ..Default::default()
                            })
                            .with_hashed_key("photos:people:name")
                            .with_action_target("photos:people:name"),
                    );
                }
                cards.extend(
                    self.people_controls
                        .iter()
                        .enumerate()
                        .map(|(index, control)| {
                            AccessNode::new(Role::Button)
                                .with_name(control.label.clone())
                                .with_bounds(if control.is_card() {
                                    control.bounds
                                } else {
                                    control.button.get_rect()
                                })
                                .with_state(AccessState {
                                    focusable: control.button.is_focusable(),
                                    focused: control.button.is_focused(),
                                    disabled: !control.button.is_enabled(),
                                    ..Default::default()
                                })
                                .with_action_target(format!("photos:people:control:{index}"))
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-people"))
                    .with_description(tr("photos-a11y-people-description"))
                    .with_bounds(self.canvas_rect)
                    .with_hashed_key("photos:people")
                    .with_children(cards)
            } else if self.library_destination == LibraryDestination::Places {
                cards.extend(self.places_hits.iter().map(|(hit, bounds)| {
                    let (name, target) = match hit {
                        PlacesHit::Place(key) => (
                            "photos-a11y-open-place",
                            format!("photos:places:open:{key}"),
                        ),
                        PlacesHit::Photo(id) => (
                            "photos-a11y-open-photo",
                            format!("photos:places:photo:{id}"),
                        ),
                    };
                    AccessNode::new(Role::Button)
                        .with_name(tr(name))
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                        .with_action_target(target)
                }));
                // These semantic controls intentionally share the same pure
                // camera operation as keyboard arrows and pointer drag. They
                // make panning available to screen readers and agents without
                // introducing coordinate-only automation.
                for (name, target) in [
                    ("photos-a11y-pan-left", "photos:places:pan-left"),
                    ("photos-a11y-pan-right", "photos:places:pan-right"),
                    ("photos-a11y-pan-up", "photos:places:pan-up"),
                    ("photos-a11y-pan-down", "photos:places:pan-down"),
                ] {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(tr(name))
                            .with_description(tr("photos-a11y-pan-description"))
                            .with_bounds(self.places_map_bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
                cards.extend(
                    self.collection_page_hits
                        .iter()
                        .map(|(hit, bounds, enabled)| {
                            collection_page_access_node(*hit, *bounds, *enabled)
                        }),
                );
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-places"))
                    .with_description(tr("photos-a11y-places-description"))
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
                                    .with_description(tr("photos-a11y-selected-description"))
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
                                    .with_description(tr("photos-a11y-in-album"))
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
                                let actions = card_action_controls(*bounds, &self.layout_theme);
                                let favorite_target = format!("photos:library-favorite:{id}");
                                children.push(
                                    AccessNode::new(Role::ToggleButton)
                                        .with_name(tr("photos-favorite"))
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
                                            tr_with(
                                                "photos-a11y-confirm-remove-named",
                                                "name",
                                                name,
                                            )
                                        } else {
                                            tr_with("photos-a11y-remove-named", "name", name)
                                        })
                                        .with_description(tr("photos-a11y-remove-description"))
                                        .with_bounds(actions.remove)
                                        .with_state(AccessState {
                                            focusable: true,
                                            ..Default::default()
                                        })
                                        .with_action_target(format!("photos:library-remove:{id}")),
                                );
                                children.push(
                                    AccessNode::new(Role::Button)
                                        .with_name(tr_with(
                                            "photos-a11y-details-named",
                                            "name",
                                            name,
                                        ))
                                        .with_description(tr("photos-a11y-details-description"))
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
                            || tr("photos-a11y-all-photos"),
                            |(_, name, count)| {
                                tr_args(
                                    "photos-people-card-name",
                                    &[
                                        ("name", ArgValue::Text(name.clone())),
                                        ("count", ArgValue::Number(*count as f64)),
                                    ],
                                )
                            },
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
                        tr("photos-favorites")
                    } else {
                        match (&card.folder, &card.person, &card.tag) {
                            (Some(folder), None, None) => {
                                tr_with("photos-a11y-filter-folder", "name", folder)
                            }
                            (None, Some(person), None) => {
                                tr_with("photos-a11y-filter-person", "name", person)
                            }
                            (None, None, Some(tag)) => {
                                tr_with("photos-a11y-filter-tag", "name", tag)
                            }
                            _ => tr("photos-a11y-all-metadata"),
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
                cards.extend(
                    self.collection_page_hits
                        .iter()
                        .map(|(hit, bounds, enabled)| {
                            collection_page_access_node(*hit, *bounds, *enabled)
                        }),
                );
                for (forward, enabled, bounds) in &self.library_page_hits {
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(if *forward {
                                tr("photos-a11y-next-photos")
                            } else {
                                tr("photos-a11y-previous-photos")
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
                        SelectionHit::PreviousDestinationPage => (
                            "photos-a11y-previous-albums",
                            "photos:selection:destination-previous",
                            self.selection_album_page == 0,
                        ),
                        SelectionHit::NextDestinationPage => (
                            "photos-a11y-more-albums",
                            "photos:selection:destination-next",
                            (self.selection_album_page + 1) * SELECTION_ALBUMS_PER_PAGE
                                >= self.library_albums.len(),
                        ),
                        SelectionHit::Start => {
                            ("photos-a11y-select", "photos:selection:start", false)
                        }
                        SelectionHit::SelectAll => {
                            ("photos-a11y-select-all", "photos:selection:all", false)
                        }
                        SelectionHit::Clear => (
                            "photos-a11y-clear-selection",
                            "photos:selection:clear",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::Favorite => (
                            if self.selection_all_favorite {
                                "photos-a11y-unfavorite-selection"
                            } else {
                                "photos-a11y-favorite-selection"
                            },
                            "photos:selection:favorite",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::EditDetails => (
                            "photos-a11y-edit-selection",
                            "photos:selection:edit-details",
                            self.selected_photo_ids.is_empty(),
                        ),
                        SelectionHit::ChooseAlbum => (
                            "photos-a11y-choose-album",
                            "photos:selection:choose-album",
                            self.selected_photo_ids.is_empty() || self.library_albums.is_empty(),
                        ),
                        SelectionHit::CancelDestinationChooser => (
                            "photos-a11y-cancel-choice",
                            "photos:selection:destination-cancel",
                            false,
                        ),
                        SelectionHit::CloseDestinationChooser => (
                            "photos-a11y-close-chooser",
                            "photos:selection:destination-close",
                            false,
                        ),
                        SelectionHit::ApplyDestination => (
                            "photos-a11y-use-album",
                            "photos:selection:destination-apply",
                            self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::AddToAlbum => (
                            "photos-a11y-add-selection",
                            "photos:selection:add-to-album",
                            self.selected_photo_ids.is_empty()
                                || self.selection_destination_album.is_none(),
                        ),
                        SelectionHit::Done => (
                            "photos-a11y-finish-selection",
                            "photos:selection:done",
                            false,
                        ),
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(tr(name))
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
                            || tr("photos-a11y-choose-album"),
                            |(_, name, _)| tr_with("photos-a11y-use-named-album", "name", name),
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
                    // The inspector owns the interaction scope. Keeping the
                    // dimmed grid's cards in the a11y tree would let a screen
                    // reader activate controls that pointer and keyboard users
                    // correctly cannot reach while editing details.
                    cards.clear();
                    let fields: &[(&str, &str, &TextInput)] = match self.metadata_editor_section {
                        MetadataEditorSection::Organize => &[
                            ("photos-folder", "folder", &self.metadata_folder_input),
                            ("photos-people", "people", &self.metadata_people_input),
                            ("photos-tags", "tags", &self.metadata_tags_input),
                        ],
                        MetadataEditorSection::TimeAndPlace => &[
                            (
                                "photos-captured",
                                "captured",
                                &self.metadata_captured_at_input,
                            ),
                            (
                                "photos-a11y-place-name",
                                "place",
                                &self.metadata_place_input,
                            ),
                            ("photos-latitude", "latitude", &self.metadata_latitude_input),
                            (
                                "photos-longitude",
                                "longitude",
                                &self.metadata_longitude_input,
                            ),
                        ],
                    };
                    for (name, target, input) in fields {
                        // The field's clear toggle, as the field draws it,
                        // routed to the same command an agent runs.
                        let clear = match *target {
                            "folder" => Some("clear-folder"),
                            "people" => Some("clear-people"),
                            "tags" => Some("clear-tags"),
                            "captured" => Some("clear-captured-at"),
                            "place" => Some("clear-location"),
                            _ => None,
                        };
                        let toggles: Vec<AccessNode> = clear
                            .and_then(|command| {
                                let toggle = ui::widget::Widget::accessibility(*input)?
                                    .children
                                    .into_iter()
                                    .next()?;
                                Some(vec![
                                    toggle.with_action_target(format!("photos:metadata:{command}")),
                                ])
                            })
                            .unwrap_or_default();
                        cards.push(
                            AccessNode::new(Role::TextInput)
                                .with_name(tr(name))
                                .with_value(input.value())
                                .with_bounds(input.get_rect())
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: input.is_focused(),
                                    ..Default::default()
                                })
                                .with_hashed_key(format!("photos:metadata:{target}"))
                                .with_action_target(format!("photos:metadata:field:{target}"))
                                .with_children(toggles),
                        );
                    }
                    for (hit, bounds) in &self.metadata_editor_hits {
                        let (name, target) = match hit {
                            MetadataEditorHit::Save => {
                                ("photos-a11y-save-details", "photos:metadata:save")
                            }
                            MetadataEditorHit::Cancel => {
                                ("photos-a11y-cancel-details", "photos:metadata:cancel")
                            }
                            MetadataEditorHit::Organize => {
                                ("photos-a11y-organize-fields", "photos:metadata:organize")
                            }
                            MetadataEditorHit::TimeAndPlace => (
                                "photos-a11y-time-place-fields",
                                "photos:metadata:time-and-place",
                            ),
                            MetadataEditorHit::ClearFolder => {
                                ("photos-clear-folder", "photos:metadata:clear-folder")
                            }
                            MetadataEditorHit::ClearPeople => {
                                ("photos-clear-people", "photos:metadata:clear-people")
                            }
                            MetadataEditorHit::ClearTags => {
                                ("photos-clear-tags", "photos:metadata:clear-tags")
                            }
                            MetadataEditorHit::ClearCapturedAt => {
                                ("photos-clear-captured", "photos:metadata:clear-captured-at")
                            }
                            MetadataEditorHit::ClearLocation => {
                                ("photos-clear-location", "photos:metadata:clear-location")
                            }
                            MetadataEditorHit::PlaceOnMap => {
                                ("photos-a11y-place-from-map", "photos:metadata:place-on-map")
                            }
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(target),
                        );
                    }
                    for (hit, button) in &self.metadata_editor_buttons {
                        let bounds = button.get_rect();
                        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
                            continue;
                        }
                        let (name, target) = match hit {
                            MetadataEditorHit::Save => {
                                ("photos-a11y-save-details", "photos:metadata:save")
                            }
                            MetadataEditorHit::Cancel => {
                                ("photos-a11y-cancel-details", "photos:metadata:cancel")
                            }
                            MetadataEditorHit::Organize => {
                                ("photos-a11y-organize-fields", "photos:metadata:organize")
                            }
                            MetadataEditorHit::TimeAndPlace => (
                                "photos-a11y-time-place-fields",
                                "photos:metadata:time-and-place",
                            ),
                            MetadataEditorHit::ClearFolder => {
                                ("photos-clear-folder", "photos:metadata:clear-folder")
                            }
                            MetadataEditorHit::ClearPeople => {
                                ("photos-clear-people", "photos:metadata:clear-people")
                            }
                            MetadataEditorHit::ClearTags => {
                                ("photos-clear-tags", "photos:metadata:clear-tags")
                            }
                            MetadataEditorHit::ClearCapturedAt => {
                                ("photos-clear-captured", "photos:metadata:clear-captured-at")
                            }
                            MetadataEditorHit::ClearLocation => {
                                ("photos-clear-location", "photos:metadata:clear-location")
                            }
                            MetadataEditorHit::PlaceOnMap => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
                                .with_bounds(bounds)
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
                            .with_name(tr("photos-a11y-album-name"))
                            .with_value(self.album_name_input.value())
                            .with_bounds(self.album_name_input.get_rect())
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.album_name_input.is_focused(),
                                ..Default::default()
                            })
                            .with_hashed_key("photos:album-editor:name"),
                    );
                    for (hit, bounds) in &self.album_editor_hits {
                        let (name, target) = match hit {
                            AlbumEditorHit::Save => {
                                ("photos-a11y-save-album", "photos:album-editor:save")
                            }
                            AlbumEditorHit::Cancel => {
                                ("photos-a11y-cancel-album", "photos:album-editor:cancel")
                            }
                            AlbumEditorHit::Delete => {
                                ("photos-delete-album", "photos:album-editor:delete")
                            }
                            AlbumEditorHit::ConfirmDelete => (
                                "photos-a11y-confirm-delete-album",
                                "photos:album-editor:confirm-delete",
                            ),
                            AlbumEditorHit::New => continue,
                            AlbumEditorHit::Edit => {
                                ("photos-edit-album", "photos:album-editor:edit")
                            }
                            AlbumEditorHit::PreviousPage => (
                                "photos-a11y-previous-photos",
                                "photos:album-editor:previous",
                            ),
                            AlbumEditorHit::NextPage => {
                                ("photos-a11y-more-photos", "photos:album-editor:next")
                            }
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
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
                            AlbumEditorHit::New => ("photos-new-album", "photos:album-editor:new"),
                            _ => continue,
                        };
                        cards.push(
                            AccessNode::new(Role::Button)
                                .with_name(tr(name))
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
                        .with_name(tr("photos-a11y-choose-album"))
                        .with_description(tr("photos-a11y-chooser-description"))
                        .with_bounds(self.canvas_rect)
                        .with_state(AccessState::new().modal(true))
                        .with_hashed_key("photos:destination-album-dialog")
                        .with_children(cards)
                } else {
                    if !self.album_editor && !self.metadata_editor {
                        if self.library_items.is_empty() {
                            // An empty library paints only its empty state:
                            // read that out, and publish no timeline index
                            // that is not on screen.
                            let (title, body) = Self::library_empty_state_text();
                            cards.push(
                                AccessNode::new(Role::StaticText)
                                    .with_name(title)
                                    .with_description(body)
                                    .with_bounds(self.canvas_rect)
                                    .with_hashed_key("photos:library-empty"),
                            );
                        } else {
                            cards.push(self.timeline_index.accessibility(
                                self.timeline_index_metrics,
                                &self.timeline_index_markers,
                            ));
                        }
                    }
                    AccessNode::new(Role::List)
                        .with_name(tr("photos-library"))
                        .with_description(tr_count("photos-count-photos", self.library_total))
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
        let mut children = vec![canvas];
        children.extend(
            self.empty_state
                .as_ref()
                .and_then(Widget::accessibility)
                .map(|node| node.with_hashed_key("photos:empty-state")),
        );
        children.extend(self.chrome_semantics());
        if let Some(notice) = self.notice.as_deref() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(notice)
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:notice"),
            );
        }
        if let Some(job) = self.directory_import.as_ref() {
            let mut status_children = vec![
                AccessNode::new(Role::StaticText)
                    .with_name(if job.mode == DirectoryImportMode::Reindex {
                        tr("photos-reindex-running")
                    } else {
                        tr("photos-import-running")
                    })
                    .with_description(
                        self.directory_import_summary()
                            .map(|(_, summary, _, _)| summary)
                            .unwrap_or_default(),
                    )
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:directory-import-progress"),
                AccessNode::new(Role::Button)
                    .with_name(tr("photos-import-stop"))
                    .with_action_target("photos:directory-import:stop")
                    .with_hashed_key("photos:directory-import-stop"),
            ];
            if !job.failures.is_empty() {
                status_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(tr("photos-import-details"))
                        .with_description(tr_count(
                            "photos-a11y-inspect-issues",
                            job.failures.len(),
                        ))
                        .with_action_target("photos:directory-import:details")
                        .with_hashed_key("photos:directory-import-details"),
                );
            }
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-a11y-import-progress"))
                    .with_state(AccessState {
                        busy: true,
                        ..Default::default()
                    })
                    .with_children(status_children)
                    .with_hashed_key("photos:directory-import-status"),
            );
        } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
            let mut status_children = vec![
                AccessNode::new(Role::StaticText)
                    .with_name(if receipt.mode == DirectoryImportMode::Reindex {
                        tr("photos-reindex-complete")
                    } else {
                        tr("photos-import-complete")
                    })
                    .with_description(
                        self.directory_import_summary()
                            .map(|(_, summary, _, _)| summary)
                            .unwrap_or_default(),
                    )
                    .with_live(Politeness::Polite)
                    .with_hashed_key("photos:directory-import-result"),
            ];
            if receipt.failure_count > 0 {
                status_children.push(
                    AccessNode::new(Role::Button)
                        .with_name(tr("photos-import-details"))
                        .with_description(tr_count(
                            "photos-a11y-inspect-issues",
                            receipt.failures.len(),
                        ))
                        .with_action_target("photos:directory-import:details")
                        .with_hashed_key("photos:directory-import-details"),
                );
            }
            status_children.push(
                AccessNode::new(Role::Button)
                    .with_name(tr("photos-import-dismiss"))
                    .with_action_target("photos:directory-import:dismiss")
                    .with_hashed_key("photos:directory-import-dismiss"),
            );
            children.push(
                AccessNode::new(Role::Group)
                    .with_name(tr("photos-a11y-import-result"))
                    .with_children(status_children)
                    .with_hashed_key("photos:directory-import-status"),
            );
        }
        if self.directory_import_details_open {
            let (failures, total) = if let Some(job) = self.directory_import.as_ref() {
                (&job.failures[..], job.failures.len())
            } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
                (&receipt.failures[..], receipt.failure_count)
            } else {
                (&[][..], 0)
            };
            let retained = failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT);
            let mut detail_children = failures
                .iter()
                .take(retained)
                .enumerate()
                .map(|(index, failure)| {
                    let name = failure
                        .source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map_or_else(|| tr("photos-import-unnamed"), str::to_owned);
                    AccessNode::new(Role::ListItem)
                        .with_name(name)
                        .with_description(tr_args(
                            "photos-a11y-issue-description",
                            &[
                                ("message", ArgValue::Text(failure.message.clone())),
                                (
                                    "next",
                                    ArgValue::Text(if failure.retryable {
                                        tr("photos-import-can-retry")
                                    } else {
                                        tr("photos-import-needs-change")
                                    }),
                                ),
                            ],
                        ))
                        .with_hashed_key(format!("photos:directory-failure:{index}"))
                })
                .collect::<Vec<_>>();
            detail_children.push(
                AccessNode::new(Role::Button)
                    .with_name(tr("photos-close"))
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
                    .with_action_target("photos:directory-import:details-close")
                    .with_hashed_key("photos:directory-import:details-close"),
            );
            children.push(
                AccessNode::new(Role::Dialog)
                    .with_name(tr("photos-import-issues-title"))
                    .with_description(tr_args(
                        "photos-import-issues-total",
                        &[
                            ("kept", ArgValue::Number(retained as f64)),
                            ("count", ArgValue::Number(total as f64)),
                        ],
                    ))
                    .with_state(AccessState {
                        modal: true,
                        ..Default::default()
                    })
                    .with_children(detail_children)
                    .with_hashed_key("photos:directory-import-dialog"),
            );
        }
        AccessNode::new(Role::Group)
            .with_name(tr("photos-app"))
            .with_bounds(self.rect)
            .with_semantic_key("photos")
            .with_children(children)
    }

    fn perform_directory_semantic_action(&mut self, target: &str, action: &AccessAction) -> bool {
        let Some(command) = target.strip_prefix("photos:directory-import:") else {
            return false;
        };
        if matches!(action, AccessAction::Focus) {
            let status_hit = match command {
                "stop" => Some(DirectoryStatusHit::Stop),
                "details" => Some(DirectoryStatusHit::Details),
                "dismiss" => Some(DirectoryStatusHit::Dismiss),
                _ => None,
            };
            if let Some(hit) = status_hit {
                let available = match hit {
                    DirectoryStatusHit::Stop => self.directory_import.is_some(),
                    DirectoryStatusHit::Details => {
                        self.directory_import
                            .as_ref()
                            .is_some_and(|job| !job.failures.is_empty())
                            || self
                                .directory_import_receipt
                                .as_ref()
                                .is_some_and(|receipt| receipt.failure_count > 0)
                    }
                    DirectoryStatusHit::Dismiss => {
                        self.directory_import.is_none() && self.directory_import_receipt.is_some()
                    }
                };
                if available {
                    for (candidate, button) in &mut self.directory_import_buttons {
                        button.set_focus(*candidate == hit);
                    }
                }
                return available;
            }
            return self.directory_import_details_open && command == "details-close";
        }
        if !matches!(action, AccessAction::Activate) {
            return false;
        }
        match command {
            "stop" => self.activate_directory_status(DirectoryStatusHit::Stop),
            "details" => self.activate_directory_status(DirectoryStatusHit::Details),
            "dismiss" => self.activate_directory_status(DirectoryStatusHit::Dismiss),
            "details-close" => self.close_directory_details(),
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
        _doc: &AutoCommit,
    ) -> bool {
        if let Some(destination) = target.strip_prefix("photos:library:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let destination = match destination {
                "timeline" => LibraryDestination::Timeline,
                "albums" => LibraryDestination::Albums,
                "people" => LibraryDestination::People,
                "places" => LibraryDestination::Places,
                "analysis" => LibraryDestination::Analysis,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) {
                self.open_nav_target(&chrome::NavTarget::Destination(destination));
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:places:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Focus) {
                return true;
            }
            if let Some(photo_id) = command.strip_prefix("photo:") {
                if self.presented().photo(photo_id).cloned().is_none() {
                    return false;
                }
                self.show_photo(photo_id);
                return true;
            }
            match command {
                "toggle-list" => self.places_list_mode = !self.places_list_mode,
                "zoom-in" => self.zoom_places_by(1.0),
                "zoom-out" => self.zoom_places_by(-1.0),
                "pan-left" => self.set_places_viewport(self.places_viewport.panned(
                    44.0,
                    0.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-right" => self.set_places_viewport(self.places_viewport.panned(
                    -44.0,
                    0.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-up" => self.set_places_viewport(self.places_viewport.panned(
                    0.0,
                    44.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                "pan-down" => self.set_places_viewport(self.places_viewport.panned(
                    0.0,
                    -44.0,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                )),
                _ => {
                    let Some(key) = command.strip_prefix("open:") else {
                        return false;
                    };
                    return self.open_place_collection(&self.presented(), key).is_ok();
                }
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:album-edit:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if self.presented().album(album_id).cloned().is_none() {
                    return false;
                }
                self.active_album = Some(album_id.to_owned());
                self.begin_edit_album();
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:collection-page:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            let hit = match command {
                "albums-previous" => CollectionPageHit::AlbumsPrevious,
                "albums-next" => CollectionPageHit::AlbumsNext,
                "people-previous" => CollectionPageHit::PeoplePrevious,
                "people-next" => CollectionPageHit::PeopleNext,
                "places-previous" => CollectionPageHit::PlacesPrevious,
                "places-next" => CollectionPageHit::PlacesNext,
                _ => return false,
            };
            if matches!(action, AccessAction::Activate) {
                self.page_collection(hit);
            }
            return true;
        }
        if target == "photos:people:name" {
            return match action {
                AccessAction::Focus => {
                    self.people_name_input.set_focus(true);
                    true
                }
                AccessAction::SetValue(value) => {
                    self.people_name_input.set_text(value);
                    true
                }
                _ => false,
            };
        }
        if let Some(index) = target
            .strip_prefix("photos:people:control:")
            .and_then(|value| value.parse::<usize>().ok())
        {
            let Some(control) = self.people_controls.get(index) else {
                return false;
            };
            return match action {
                AccessAction::Focus => {
                    for item in &mut self.people_controls {
                        item.button.set_focus(false);
                    }
                    if let Some(control) = self.people_controls.get_mut(index) {
                        control.button.set_focus(true);
                    }
                    true
                }
                AccessAction::Activate if control.button.is_enabled() => {
                    let intent = control.intent.clone();
                    self.handle_people_intent(intent);
                    true
                }
                _ => false,
            };
        }
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
                return self.apply_timeline_index_intent(&intent);
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
        if let Some(command) = target.strip_prefix("photos:analysis:") {
            if let Some(photo_id) = command.strip_prefix("failure:") {
                if matches!(action, AccessAction::Focus) {
                    return self
                        .library_analysis
                        .failures()
                        .any(|failure| failure.photo_id == photo_id);
                }
                if !matches!(action, AccessAction::Activate)
                    || !self
                        .library_analysis
                        .failures()
                        .any(|failure| failure.photo_id == photo_id)
                {
                    return false;
                }
                self.show_photo(photo_id);
                return true;
            }
            let hit = match command {
                "start" => LibraryAnalysisHit::Start,
                "pause" => LibraryAnalysisHit::Pause,
                "resume" => LibraryAnalysisHit::Resume,
                "retry" => LibraryAnalysisHit::RetryFailures,
                _ => return false,
            };
            if matches!(action, AccessAction::Focus) {
                for (candidate, button) in &mut self.library_analysis_buttons {
                    button.set_focus(*candidate == hit);
                }
                return true;
            }
            return matches!(action, AccessAction::Activate) && self.activate_library_analysis(hit);
        }
        if let Some(command) = target.strip_prefix("photos:selection:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                if let Some(album_id) = command.strip_prefix("destination:") {
                    if self.presented().album(album_id).cloned().is_none() {
                        self.notice = Some(tr("photos-album-gone"));
                        return false;
                    }
                    self.selection_destination_album = Some(album_id.to_owned());
                    self.notice = Some(tr("photos-selection-album-chosen"));
                    return true;
                }
                let hit = match command {
                    "start" => SelectionHit::Start,
                    "all" => SelectionHit::SelectAll,
                    "clear" => SelectionHit::Clear,
                    "favorite" => SelectionHit::Favorite,
                    "edit-details" => SelectionHit::EditDetails,
                    "choose-album" => SelectionHit::ChooseAlbum,
                    "destination-previous" => SelectionHit::PreviousDestinationPage,
                    "destination-next" => SelectionHit::NextDestinationPage,
                    "destination-cancel" => SelectionHit::CancelDestinationChooser,
                    "destination-close" => SelectionHit::CloseDestinationChooser,
                    "destination-apply" => SelectionHit::ApplyDestination,
                    "add-to-album" => SelectionHit::AddToAlbum,
                    "done" => SelectionHit::Done,
                    _ => return false,
                };
                self.handle_selection_hit(hit);
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
                "new" => self.begin_new_album(),
                "edit" => self.begin_edit_album(),
                "previous-page" if self.can_page_library(false) => self.page_library(false),
                "next-page" if self.can_page_library(true) => self.page_library(true),
                "save" => self.save_album_editor(),
                "cancel" => self.cancel_album_editor(),
                "delete" => self.album_delete_confirm = true,
                "confirm-delete" => self.delete_active_album(),
                _ => return false,
            }
            return true;
        }
        if let Some(command) = target.strip_prefix("photos:metadata:") {
            if let Some(field) = command.strip_prefix("field:") {
                // Organize and Time & Place fields share the same focus slots.
                let focus = match field {
                    "folder" | "captured" => 0,
                    "people" | "place" => 1,
                    "tags" | "latitude" => 2,
                    "longitude" => 3,
                    _ => return false,
                };
                return match action {
                    AccessAction::Focus => {
                        self.set_metadata_editor_focus(focus);
                        true
                    }
                    AccessAction::SetValue(value) => {
                        match field {
                            "folder" => self.metadata_folder_input.set_text(value),
                            "people" => self.metadata_people_input.set_text(value),
                            "tags" => self.metadata_tags_input.set_text(value),
                            "captured" => self.metadata_captured_at_input.set_text(value),
                            "place" => self.metadata_place_input.set_text(value),
                            "latitude" => {
                                self.metadata_latitude_input.set_text(value);
                                self.metadata_location_from_map = false;
                            }
                            "longitude" => {
                                self.metadata_longitude_input.set_text(value);
                                self.metadata_location_from_map = false;
                            }
                            _ => return false,
                        }
                        true
                    }
                    _ => false,
                };
            }
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "save" => self.save_metadata_editor(),
                "cancel" => self.cancel_metadata_editor(),
                "organize" => self.set_metadata_editor_section(MetadataEditorSection::Organize),
                "time-and-place" => {
                    self.set_metadata_editor_section(MetadataEditorSection::TimeAndPlace);
                }
                "clear-folder" => self.clear_metadata_dimension(0),
                "clear-people" => self.clear_metadata_dimension(1),
                "clear-tags" => self.clear_metadata_dimension(2),
                "clear-captured-at" => self.clear_metadata_dimension(3),
                "clear-location" => self.clear_metadata_dimension(4),
                "place-on-map" => {
                    let bounds = self.metadata_location_map_bounds;
                    let _ = self.set_metadata_location_from_map_point(bounds.center());
                }
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
            self.library_destination = LibraryDestination::Timeline;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
            self.library_offset = 0;
            self.timeline_scroll_y = 0.0;
            return true;
        }
        if let Some(id) = target.strip_prefix("photos:library-details:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                self.begin_metadata_editor(id);
            }
            return true;
        }
        if target == "photos:library-favorites" {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            if matches!(action, AccessAction::Activate) {
                let _ = self.view_favorites(&self.presented());
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
                self.commit(PhotoLibrary::REMOVE_PHOTO.with(Id::from_raw(photo_id)));
                self.library_remove_confirm = None;
                self.notice = Some(tr("photos-removed"));
            } else {
                self.library_remove_confirm = Some(photo_id.to_owned());
                self.notice = Some(tr("photos-remove-confirm"));
            }
        } else if set_favorite {
            self.commit(crate::model::photo_paths(photo_id).favorite.set(!favorite));
        } else if self.album_editor {
            if !self.album_editor_members.insert(photo_id.to_owned()) {
                self.album_editor_members.remove(photo_id);
            }
        } else {
            self.show_photo(photo_id);
        }
        true
    }

    fn begin_directory_import(
        &mut self,
        path: std::path::PathBuf,
        invocation: Option<tilo_actions::InvocationId>,
        mode: DirectoryImportMode,
        retain_exif_location: bool,
    ) {
        if self.directory_import.is_some() {
            self.notice = Some(tr("photos-import-already-running"));
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let content = self.content.clone();
        let spawn = ui::rt::spawn_thread("photos-directory-import", move || {
            let result = crate::import::index_directory(
                &path,
                &content,
                crate::import::ImportOptions {
                    retain_exif_location,
                    ..crate::import::ImportOptions::default()
                },
                &worker_cancel,
                |progress| {
                    let _ = sender.send(DirectoryImportEvent::Progress(progress));
                },
            );
            let _ = sender.send(DirectoryImportEvent::Finished(result));
        });
        if spawn.is_err() {
            self.notice = Some(tr("photos-import-could-not-start"));
            if let Some(invocation) = invocation {
                self.settle(tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "photos.directory_worker_unavailable".into(),
                        message: tr("photos-import-could-not-start"),
                        details: None,
                        retryable: true,
                    },
                });
            }
            return;
        }
        self.directory_import_receipt = None;
        self.directory_import_details_open = false;
        self.directory_import_details_scroll.set_offset(Vec2::ZERO);
        self.directory_import = Some(DirectoryImport {
            receiver,
            cancel,
            invocation,
            mode,
            discovered: 0,
            imported: 0,
            refreshed: 0,
            unchanged: 0,
            skipped: 0,
            sequence: 0,
            pending: VecDeque::new(),
            failures: Vec::new(),
            worker_done: false,
            cancelled: false,
            limit_reached: false,
            produced_resources: Vec::new(),
            _shot_sender: None,
        });
        self.notice = Some(match mode {
            DirectoryImportMode::Import => tr("photos-import-preparing"),
            DirectoryImportMode::Reindex => tr("photos-reindex-preparing"),
        });
    }

    fn stop_directory_import(&mut self) -> bool {
        let Some(job) = self.directory_import.as_mut() else {
            return false;
        };
        job.cancel.store(true, Ordering::Release);
        job.cancelled = true;
        job.pending.clear();
        if let Some(invocation) = job.invocation.take() {
            self.settle(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some(tr("photos-import-stopped-notice")),
            });
        }
        self.resource_invocation = None;
        self.notice = Some(tr("photos-import-stopping"));
        true
    }

    fn activate_directory_status(&mut self, hit: DirectoryStatusHit) -> bool {
        match hit {
            DirectoryStatusHit::Stop => self.stop_directory_import(),
            DirectoryStatusHit::Details => {
                let has_failures = self
                    .directory_import
                    .as_ref()
                    .is_some_and(|job| !job.failures.is_empty())
                    || self
                        .directory_import_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.failure_count > 0);
                if has_failures {
                    self.directory_import_details_open = true;
                    self.directory_import_details_scroll.set_offset(Vec2::ZERO);
                }
                has_failures
            }
            DirectoryStatusHit::Dismiss => {
                if self.directory_import.is_some() || self.directory_import_receipt.is_none() {
                    return false;
                }
                self.directory_import_receipt = None;
                self.directory_import_details_open = false;
                self.directory_import_details_layout = None;
                true
            }
        }
    }

    fn close_directory_details(&mut self) -> bool {
        if !self.directory_import_details_open {
            return false;
        }
        self.directory_import_details_open = false;
        self.directory_import_details_layout = None;
        true
    }

    #[expect(
        clippy::too_many_lines,
        reason = "applies every directory import event kind and the final summary from one worker drain"
    )]
    fn drain_directory_import(&mut self) {
        if self.directory_import.is_none() {
            return;
        }
        loop {
            let event = self
                .directory_import
                .as_ref()
                .map(|job| job.receiver.try_recv());
            match event {
                None => return,
                Some(Ok(DirectoryImportEvent::Progress(progress))) => {
                    let Some(job) = self.directory_import.as_mut() else {
                        return;
                    };
                    job.discovered = progress.discovered;
                    job.skipped = progress.skipped;
                    job.sequence = job.sequence.saturating_add(1);
                    let (invocation, sequence) = (job.invocation, job.sequence);
                    if let Some(invocation) = invocation {
                        self.settle(tilo_actions::InvocationUpdate::Progress {
                            invocation,
                            sequence,
                            phase: "indexing".into(),
                            completed: Some(progress.indexed as u64),
                            total: None,
                            message: Some(tr_args(
                                "photos-import-found-progress",
                                &[
                                    ("found", ArgValue::Number(progress.discovered as f64)),
                                    ("issues", ArgValue::Number(progress.skipped as f64)),
                                ],
                            )),
                        });
                    }
                }
                Some(Ok(DirectoryImportEvent::Finished(Ok(report)))) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        job.discovered = report.discovered;
                        job.skipped = report.failures.len();
                        let cancellation_was_requested = job.cancelled;
                        job.cancelled |= report.cancelled;
                        job.limit_reached = report.limit_reached;
                        job.failures = report.failures;
                        if !cancellation_was_requested {
                            job.pending = report.photos.into();
                        }
                        job.worker_done = true;
                    }
                    break;
                }
                Some(Ok(DirectoryImportEvent::Finished(Err(message)))) => {
                    let Some(mut job) = self.directory_import.take() else {
                        return;
                    };
                    self.notice = Some(message.clone());
                    if let Some(invocation) = job.invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_index_failed".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                    return;
                }
                Some(Err(std::sync::mpsc::TryRecvError::Empty)) => break,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    let Some(mut job) = self.directory_import.take() else {
                        return;
                    };
                    let message = tr("photos-import-worker-stopped");
                    self.notice = Some(message.clone());
                    if let Some(invocation) = job.invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "photos.directory_worker_stopped".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        });
                    }
                    return;
                }
            }
        }

        for _ in 0..DIRECTORY_IMPORT_PER_FRAME {
            let indexed = self
                .directory_import
                .as_mut()
                .and_then(|job| job.pending.pop_front());
            let Some(indexed) = indexed else { break };
            let existing = self.presented().photo(&indexed.photo.id).cloned();
            let mode = self
                .directory_import
                .as_ref()
                .map_or(DirectoryImportMode::Import, |job| job.mode);
            let (action, changed) = match (mode, existing.as_ref()) {
                (DirectoryImportMode::Reindex, Some(current)) => {
                    let capture_changes =
                        current.captured_at.is_none() && indexed.photo.captured_at.is_some();
                    let location_changes = current.location.as_ref().is_none_or(|location| {
                        location.provenance.source == "exif"
                            && current.location != indexed.photo.location
                    });
                    (
                        PhotoLibrary::REFRESH_IMPORTED_METADATA.with((
                            indexed.photo.item(),
                            indexed.photo.captured_at.clone(),
                            indexed.photo.location.clone(),
                        )),
                        capture_changes || location_changes,
                    )
                }
                (_, Some(_)) => (crate::resource::import(&indexed.photo), false),
                (_, None) => (crate::resource::import(&indexed.photo), true),
            };
            if !changed {
                if let Some(job) = self.directory_import.as_mut() {
                    job.unchanged = job.unchanged.saturating_add(1);
                }
                continue;
            }
            match self.submit(action) {
                Ok(()) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        if existing.is_some() {
                            job.refreshed = job.refreshed.saturating_add(1);
                        } else {
                            job.imported = job.imported.saturating_add(1);
                        }
                        job.produced_resources.push(indexed.photo.id);
                    }
                }
                Err(error) => {
                    if let Some(job) = self.directory_import.as_mut() {
                        job.skipped = job.skipped.saturating_add(1);
                        job.failures.push(crate::import::ImportFailure {
                            source: indexed.source,
                            message: error,
                            retryable: false,
                        });
                    }
                }
            }
        }

        if let Some(mut job) = self
            .directory_import
            .take_if(|job| job.worker_done && job.pending.is_empty())
        {
            let message = tr_args(
                if job.mode == DirectoryImportMode::Import {
                    "photos-import-finished-notice"
                } else {
                    "photos-reindex-finished-notice"
                },
                &[
                    ("imported", ArgValue::Number(job.imported as f64)),
                    ("issues", ArgValue::Number(job.failures.len() as f64)),
                ],
            );
            self.notice = Some(message);
            let failure_count = job.failures.len();
            self.directory_import_receipt = Some(DirectoryImportReceipt {
                mode: job.mode,
                discovered: job.discovered,
                imported: job.imported,
                refreshed: job.refreshed,
                unchanged: job.unchanged,
                failures: job
                    .failures
                    .iter()
                    .take(DIRECTORY_FAILURE_DETAIL_LIMIT)
                    .cloned()
                    .collect(),
                failure_count,
                cancelled: job.cancelled,
                limit_reached: job.limit_reached,
            });
            if let Some(invocation) = job.invocation.take() {
                let failures: Vec<_> = job
                    .failures
                    .iter()
                    .take(50)
                    .map(|failure| serde_json::json!({
                        "name": failure.source.file_name().and_then(|name| name.to_str()).map_or_else(|| tr("photos-import-unnamed"), str::to_owned),
                        "message": failure.message,
                        "retryable": failure.retryable,
                    }))
                    .collect();
                self.settle(if job.cancelled {
                    tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(tr("photos-import-cancelled-notice")),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!({
                                "operation": if job.mode == DirectoryImportMode::Import {
                                    "import"
                                } else {
                                    "reindex"
                                },
                                "discovered": job.discovered,
                                "imported": job.imported,
                                "refreshed": job.refreshed,
                                "unchanged": job.unchanged,
                                "failed": job.failures.len(),
                                "limit_reached": job.limit_reached,
                                "failures": failures,
                            })),
                            resources: job.produced_resources,
                        },
                    }
                });
            }
            self.resource_invocation = None;
        }
    }

    /// Collect a chosen file from the portal, if one has arrived.
    #[expect(
        clippy::too_many_lines,
        reason = "drains every portal picker channel with its outcome handling side by side"
    )]
    fn drain_picker(&mut self) {
        self.drain_contents_picker();
        if let Some(rx) = self.directory_picker.as_ref() {
            match rx.try_recv() {
                Ok(ui::portal::PortalOutcome::Opened(path)) => {
                    self.directory_picker = None;
                    let invocation = self.resource_invocation.take();
                    self.begin_directory_import(
                        path,
                        invocation,
                        DirectoryImportMode::Import,
                        self.directory_picker_retain_exif_location,
                    );
                }
                Ok(ui::portal::PortalOutcome::Cancelled) => {
                    self.directory_picker = None;
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some(tr("photos-import-pick-cancelled")),
                        });
                    }
                }
                Ok(ui::portal::PortalOutcome::Failed(message)) => {
                    self.directory_picker = None;
                    self.notice = Some(tr_with("photos-import-pick-failed", "reason", &message));
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
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
                self.import_content = None;
                if self.picker_stale {
                    // The chooser already ran. Settle its real selection outcome
                    // without importing into content changed since dispatch.
                    let message = tr_with(
                        "photos-import-stale-named",
                        "name",
                        &path.file_name().map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        ),
                    );
                    self.notice = Some(message.clone());
                    if let Some(invocation) = self.resource_invocation.take() {
                        self.settle(tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
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
                self.open_path(&path);
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(if self.notice.is_none() {
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
                    self.settle(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(tr("photos-open-cancelled")),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(tilo_actions::InvocationUpdate::Failed {
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
                self.notice = Some(tr("photos-picker-failed"));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.settle(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "photos.portal_disconnected".into(),
                            message: tr("photos-picker-failed"),
                            details: None,
                            retryable: true,
                        },
                    });
                }
            }
        }
    }

    /// The photograph's shared details as label/value rows: when and where
    /// it was taken, who and what is in it. The name and size are already
    /// in the header and the status bar.
    fn viewer_detail_rows(details: &ViewerDetails) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        if let Some(description) = details.description.as_deref() {
            rows.push((tr("photos-description"), description.to_owned()));
        }
        if let Some(captured_at) = details.captured_at.as_deref() {
            rows.push((tr("photos-captured"), captured_label(captured_at)));
        }
        if let Some(place) = details.place.as_deref() {
            rows.push((tr("photos-place"), place.to_owned()));
        }
        if !details.people.is_empty() {
            rows.push((tr("photos-people"), details.people.join(", ")));
        }
        if !details.tags.is_empty() {
            rows.push((tr("photos-tags"), details.tags.join(", ")));
        }
        if let Some(folder) = details.folder.as_deref() {
            rows.push((tr("photos-folder"), folder.to_owned()));
        }
        rows
    }

    /// Show the photograph's shared details on the floating material over
    /// the picture itself, inside the bounds the picture is drawn in. Chrome
    /// over media is a floating panel; the picture keeps everything else.
    fn draw_viewer_details(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let (Some(details), Some(viewer), Some(rendered)) = (
            self.viewer_details.as_ref(),
            self.viewer.as_ref(),
            self.rendered.as_ref(),
        ) else {
            return;
        };
        let rows = Self::viewer_detail_rows(details);
        if rows.is_empty() {
            return;
        }
        let Some(image) = displayed_image_rect(
            viewer,
            f64::from(rendered.rgba.w),
            f64::from(rendered.rgba.h),
        ) else {
            return;
        };
        let inset = f64::from(theme.tokens.spacing.s3);
        let pad = f64::from(theme.tokens.spacing.s4);
        let width =
            (image.width() - 2.0 * inset).min(f64::from(theme.tokens.metrics.panel_width_list));
        // A picture too small to hold the panel keeps all its pixels; the
        // details stay in its accessibility description.
        if width < f64::from(theme.tokens.metrics.panel_width_utility) * 0.75 {
            return;
        }
        let property_rows: Vec<PropertyRow<'_>> = rows
            .iter()
            .map(|(label, value)| PropertyRow::new(label, value))
            .collect();
        let face = PropertyListFace::new(&property_rows);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let body_h = face.height(text_renderer, &ink, width - 2.0 * pad);
        let height = body_h + 2.0 * pad;
        // The details never take over the picture: where they would cover
        // more than a quarter of it, the picture keeps its pixels and the
        // details stay in its accessibility description.
        if height > image.height() - 2.0 * inset || width * height > image.area() / 4.0 {
            return;
        }
        let panel = Rect::new(
            image.x0 + inset,
            image.y1 - inset - height,
            image.x0 + inset + width,
            image.y1 - inset,
        );
        ui::widget::floating_surface::paint(scene, transform, panel, theme);
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, &ink),
            panel.inset(-pad),
        );
    }

    fn refresh_library_index(&self) -> Result<(), ui::search::SearchCatalogError> {
        #[cfg(test)]
        if self.force_search_refresh_error {
            return Err(ui::search::SearchCatalogError::NonCanonicalResult);
        }
        self.library_index.sync(&self.presented())
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

    /// What the library shows when it has no photographs to lay out: still
    /// searching, nothing imported yet, or no match for the query.
    fn library_empty_state_text() -> (String, String) {
        (
            tr("photos-library-empty-title"),
            tr("photos-library-empty-body"),
        )
    }

    /// Keep the actual shared empty-state control mounted before painting,
    /// so keyboard, accessibility and automation reach the same import door.
    fn refresh_empty_state(&mut self, doc: &AutoCommit) {
        let key = self.content_offers_open().then(|| {
            let (title, body) = if self.library_mode {
                Self::library_empty_state_text()
            } else {
                self.empty_state_text()
            };
            (
                title,
                body,
                self.canvas_state == CanvasState::Error && !self.library_mode,
            )
        });
        if key != self.empty_state_key {
            self.empty_state = key.as_ref().map(|(title, body, error)| {
                let resource = ui::tilo_resource::publish::<crate::model::PhotoLibrary>(
                    "photos.library",
                    &tr("photos-act-photo-library"),
                    &<PhotoLibrary as ui::tilo_resource::Resource>::read(doc).unwrap_or_default(),
                );
                let empty = if *error {
                    EmptyState::error(title, body)
                } else {
                    EmptyState::new(title).with_message(body)
                }
                .with_icon(Icon::Image)
                .with_action(tr("photos-open"))
                .with_action_id("photos-empty-open");
                match tilo_actions::ActionBinding::unit_for(&resource, "import") {
                    Ok(binding) => empty.on_action(binding),
                    Err(_) => empty,
                }
            });
            self.empty_state_key = key;
            self.structure_dirty = true;
        }
        if let Some(empty) = &mut self.empty_state {
            empty.sync(doc, &automerge::ROOT);
        }
    }

    /// Nothing to show in the content column: the shared empty state, with
    /// one action to open a photograph where that is the way forward.
    fn draw_empty_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        title: &str,
        body: &str,
    ) {
        if let Some(empty) = &mut self.empty_state {
            empty.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &PresenceCtx::empty(),
            );
            return;
        }
        EmptyState::paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.canvas_rect,
            &EmptyStateContent {
                icon: Some(Icon::Image),
                illustration: None,
                title,
                message: Some(body),
                action_label: None,
                error: self.canvas_state == CanvasState::Error && !self.library_mode,
            },
            self.pointer,
        );
    }

    /// One line of `role` under `theme`.
    fn line_of(theme: &Theme, role: ui::TextRole) -> f64 {
        f64::from(theme.text(role).line_height_px())
    }

    /// The side and gap of the People surface's cards at `width`.
    fn people_grid(theme: &Theme, width: f64) -> (usize, f64, f64) {
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        let extent = ui::widget::MediaSize::Small.extent(theme);
        let columns = ui::widget::card_columns(width, extent, gap).max(1);
        (
            columns,
            ui::widget::card_column_width(width, columns, gap),
            gap,
        )
    }

    /// Place the People surface: the recognition section and its controls,
    /// then the people you named and the suggestions to review, which scroll
    /// under the section; the photographs, the name field and the decisions
    /// in a person's detail. Controls are placed with their measured widths,
    /// so paint and hit-testing read the same rects.
    #[expect(
        clippy::too_many_lines,
        reason = "the overview and the detail are laid out in one pass so hit targets and paint agree"
    )]
    fn layout_people_controls(&mut self, theme: &Theme) {
        let frame = self.content_frame(theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let (related, sibling, group) = (
            f64::from(rhythm.related),
            f64::from(rhythm.sibling),
            f64::from(rhythm.group),
        );
        let heading = Self::line_of(theme, ui::TextRole::Heading);
        let body = Self::line_of(theme, ui::TextRole::Body);
        let caption = Self::line_of(theme, ui::TextRole::Caption);
        let control_h = ui::widget::ControlSize::Regular.height(theme);
        let mut layout = PeopleLayout::default();
        self.people_name_bounds = Rect::ZERO;
        for control in &mut self.people_controls {
            control.bounds = Rect::ZERO;
        }

        if self.people_selection.is_none() {
            let mut y = frame.y0;
            layout.recognition_heading = Some(Point::new(frame.x0, y));
            y += heading + related;
            layout.recognition_text = Rect::new(frame.x0, y, frame.x1, y + body);
            y += body + sibling;
            // The recognition controls lead the row; deleting the face data
            // stands apart at its trailing end, or under them where the row
            // has no room for it.
            let mut x = frame.x0;
            let mut row_y = y;
            for control in &mut self.people_controls {
                match control.intent {
                    PeopleIntent::ToggleRecognition
                    | PeopleIntent::Scan
                    | PeopleIntent::CancelScan => {
                        control.bounds = Rect::new(x, row_y, x + control.width, row_y + control_h);
                        x = control.bounds.x1 + related;
                    }
                    PeopleIntent::DeleteLocalData | PeopleIntent::ConfirmDeleteLocalData => {
                        let fits = x + group + control.width <= frame.x1;
                        if !fits {
                            row_y += control_h + related;
                        }
                        let x0 = if fits {
                            frame.x1 - control.width
                        } else {
                            frame.x0
                        };
                        control.bounds =
                            Rect::new(x0, row_y, x0 + control.width, row_y + control_h);
                    }
                    _ => {}
                }
            }
            y = row_y + control_h + group;

            // People and suggestions side by side where two lanes of cards
            // fit, one above the other where they do not; both scroll in the
            // rest of the column, laid out from its top and moved by the
            // scroll offset afterwards.
            let lanes_top = y;
            let viewport = Rect::new(frame.x0, lanes_top, frame.x1, self.canvas_rect.y1);
            let extent = ui::widget::MediaSize::Small.extent(theme);
            let side_by_side = frame.width() >= 2.0 * extent + group;
            let lanes = if side_by_side {
                let mid = frame.x0 + (frame.width() - group) / 2.0;
                [(frame.x0, mid), (mid + group, frame.x1)]
            } else {
                [(frame.x0, frame.x1), (frame.x0, frame.x1)]
            };
            let label_h = related + body + 2.0 * caption;
            let mut y = 0.0;
            let mut lanes_bottom = 0.0_f64;
            let mut placed: Vec<(usize, Rect)> = Vec::new();
            let mut headings = Vec::with_capacity(2);
            let mut empties = Vec::with_capacity(2);
            for (is_member, (x0, x1)) in [
                PeopleIntent::OpenApproved(String::new()),
                PeopleIntent::OpenSuggestion(String::new()),
            ]
            .into_iter()
            .zip(lanes)
            {
                if side_by_side {
                    y = 0.0;
                }
                let (columns, side, gap) = Self::people_grid(theme, x1 - x0);
                let card_h = side + label_h;
                headings.push(Point::new(x0, y));
                y += heading + related;
                let mut index = 0usize;
                for (position, control) in self.people_controls.iter().enumerate() {
                    if std::mem::discriminant(&control.intent) != std::mem::discriminant(&is_member)
                    {
                        continue;
                    }
                    let column = index % columns;
                    let row = index / columns;
                    let x = x0 + column as f64 * (side + gap);
                    let top = y + row as f64 * (card_h + group);
                    placed.push((position, Rect::new(x, top, x + side, top + card_h)));
                    index += 1;
                }
                if index == 0 {
                    empties.push(Some(Rect::new(x0, y, x1, y + 2.0 * body)));
                    y += 2.0 * body + group;
                } else {
                    empties.push(None);
                    y += index.div_ceil(columns) as f64 * (card_h + group);
                }
                lanes_bottom = lanes_bottom.max(y);
            }
            // The last row's gap becomes the page's bottom margin.
            let content_h = lanes_bottom - group + f64::from(rhythm.section);
            self.people_scroll = self
                .people_scroll
                .clamp(0.0, (content_h - viewport.height()).max(0.0));
            let shift = Vec2::new(0.0, lanes_top - self.people_scroll);
            for (position, rect) in placed {
                if let Some(control) = self.people_controls.get_mut(position) {
                    control.bounds = rect + shift;
                }
            }
            let mut headings = headings.into_iter().map(|at| at + shift);
            layout.people_heading = headings.next();
            layout.suggestions_heading = headings.next();
            let mut empties = empties
                .into_iter()
                .map(|rect| rect.map(|rect| rect + shift));
            layout.people_empty = empties.next().flatten();
            layout.suggestions_empty = empties.next().flatten();
            layout.lanes = viewport;
            layout.lanes_content = content_h;
        } else {
            let mut y = frame.y0;
            let gap = f64::from(rhythm.related);
            let side = ((frame.width() - 3.0 * gap) / 4.0)
                .min(ui::widget::MediaSize::Small.extent(theme))
                .max(1.0);
            layout.detail_photos = (0..4)
                .map(|index| {
                    let x = frame.x0 + f64::from(index) * (side + gap);
                    Rect::new(x, y, x + side, y + side)
                })
                .filter(|rect| rect.x1 <= frame.x1 + 0.5)
                .collect();
            y += side + group;
            // A named person's name is the header's title; a suggestion asks
            // who it is, and merging says what is being merged.
            let titled = self.people_merge_mode
                || matches!(self.people_selection, Some(PeopleSelection::Suggestion(_)));
            if titled {
                layout.detail_title = Some(Point::new(frame.x0, y));
                y += Self::line_of(theme, ui::TextRole::Title) + f64::from(rhythm.tight);
            }
            layout.detail_text = Rect::new(frame.x0, y, frame.x1, y + 2.0 * body);
            y += 2.0 * body + group;
            let hidden = matches!(
                &self.people_selection,
                Some(PeopleSelection::Suggestion(id))
                    if self
                        .people_controller
                        .cluster(id)
                        .is_none_or(|cluster| cluster.hidden || cluster.rejected)
            );
            if !self.people_merge_mode && !hidden {
                layout.name_label = Some(Point::new(frame.x0, y));
                y += Self::line_of(theme, ui::TextRole::Label) + f64::from(rhythm.tight);
                let field_w = frame
                    .width()
                    .min(f64::from(theme.tokens.metrics.panel_width_list));
                self.people_name_bounds = Rect::new(frame.x0, y, frame.x0 + field_w, y + control_h);
                y += control_h + sibling;
            }
            let mut x = frame.x0;
            for control in &mut self.people_controls {
                if self.people_merge_mode {
                    control.bounds =
                        Rect::new(frame.x0, y, frame.x0 + control.width, y + control_h);
                    y += control_h + related;
                } else {
                    if x > frame.x0 && x + control.width > frame.x1 {
                        x = frame.x0;
                        y += control_h + related;
                    }
                    control.bounds = Rect::new(x, y, x + control.width, y + control_h);
                    x = control.bounds.x1 + related;
                }
            }
        }
        self.people_layout = layout;
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
    #[expect(
        clippy::too_many_lines,
        reason = "the canvas layout dispatch keeps each surface's retained widget nodes together"
    )]
    fn build_canvas_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let people_surface = self.library_mode
            && (self.library_destination == LibraryDestination::People || self.people_mode);
        let canvas = if people_surface {
            self.layout_people_controls(theme);
            let mut children = Vec::new();
            let show_name = self.people_selection.is_some() && !self.people_merge_mode;
            if show_name {
                let field = self.people_name_input.build_layout(taffy, theme, node_map);
                let rect = self.people_name_bounds;
                let _ = taffy.set_style(
                    field,
                    Style {
                        position: Position::Absolute,
                        inset: taffy::Rect {
                            left: LengthPercentageAuto::length(
                                (rect.x0 - self.canvas_rect.x0) as f32,
                            ),
                            top: LengthPercentageAuto::length(
                                (rect.y0 - self.canvas_rect.y0) as f32,
                            ),
                            right: LengthPercentageAuto::auto(),
                            bottom: LengthPercentageAuto::auto(),
                        },
                        size: Size {
                            width: Dimension::length(rect.width() as f32),
                            height: Dimension::length(rect.height() as f32),
                        },
                        ..Default::default()
                    },
                );
                children.push(field);
            }
            for control in &mut self.people_controls {
                let node = control.button.build_layout(taffy, theme, node_map);
                // A card opens by itself; its button carries only the
                // control's name and action, at no size.
                let rect = if control.is_card() {
                    Rect::new(
                        self.canvas_rect.x0,
                        self.canvas_rect.y0,
                        self.canvas_rect.x0,
                        self.canvas_rect.y0,
                    )
                } else {
                    control.bounds
                };
                // A control's width is left to the button, which measures
                // its own label; the surface reads it back after layout.
                let width = if control.is_card() {
                    Dimension::length(0.0)
                } else {
                    Dimension::auto()
                };
                let _ = taffy.set_style(
                    node,
                    Style {
                        position: Position::Absolute,
                        inset: taffy::Rect {
                            left: LengthPercentageAuto::length(
                                (rect.x0 - self.canvas_rect.x0) as f32,
                            ),
                            top: LengthPercentageAuto::length(
                                (rect.y0 - self.canvas_rect.y0) as f32,
                            ),
                            right: LengthPercentageAuto::auto(),
                            bottom: LengthPercentageAuto::auto(),
                        },
                        size: Size {
                            width,
                            height: Dimension::length(rect.height() as f32),
                        },
                        ..Default::default()
                    },
                );
                children.push(node);
            }
            taffy
                .new_with_children(
                    Style {
                        position: Position::Relative,
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::percent(1.0),
                        },
                        ..Default::default()
                    },
                    &children,
                )
                .unwrap()
        } else if let Some(empty) = &mut self.empty_state {
            empty.build_layout(taffy, theme, node_map)
        } else {
            match self.viewer.as_mut() {
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
            }
        };
        // The photograph is media: it takes the whole content column, and
        // the library surfaces lay out their own gutters.
        let inset = 0.0;
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
                        left: LengthPercentageAuto::length(inset),
                        right: LengthPercentageAuto::length(inset),
                        top: LengthPercentageAuto::length(inset),
                        bottom: LengthPercentageAuto::length(inset),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        canvas
    }

    /// Lay out the status bar's band: zero height with nothing to say (a band
    /// kept anyway would leave a rule and a strip of empty chrome under an
    /// empty state), else `status_bar::height`.
    #[allow(
        clippy::unwrap_used,
        reason = "this taffy node is freshly built with a fixed, valid style"
    )]
    fn build_status_node(&self, taffy: &mut TaffyTree<()>, theme: &Theme) -> NodeId {
        let height = if self.has_status() {
            ui::chrome::status_bar::height(theme)
        } else {
            0.0
        };
        taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(height as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap()
    }

    fn draw_collection_pager(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        bounds: Rect,
        page: usize,
        total: usize,
        previous: CollectionPageHit,
        next: CollectionPageHit,
    ) {
        let theme = ctx.theme;
        self.collection_page_hits.clear();
        let page_size = COLLECTION_PAGE_SIZE;
        let pages = total.max(1).div_ceil(page_size);
        if pages <= 1 {
            return;
        }
        let previous_enabled = page > 0;
        let next_enabled = page + 1 < pages;
        let presence = PresenceCtx::empty();
        let height = ui::widget::ControlSize::Regular.height(theme);
        let label = tr_args(
            "photos-page-of",
            &[
                ("page", ArgValue::Number((page + 1) as f64)),
                ("pages", ArgValue::Number(pages as f64)),
            ],
        );
        let label_w = ctx.measure_role(&label, ui::TextRole::Caption, None).width;
        let gap = f64::from(theme.tokens.spacing.s3);
        let mut x = bounds.x1;
        let y0 = bounds.y0 + (bounds.height() - height) / 2.0;
        let mut rects = Vec::new();
        for (hit, enabled) in [(next, next_enabled), (previous, previous_enabled)] {
            if let Some((_, button)) = self
                .collection_page_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                let label = tr(if hit == previous {
                    "photos-previous"
                } else {
                    "photos-next"
                });
                let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, theme);
                let rect = Rect::new(x - width, y0, x, y0 + height);
                x = rect.x0 - gap;
                button.set_enabled(enabled);
                button.apply_layout_external(rect);
                button.draw(ctx.scene, ctx.transform, ctx.text, theme, &presence);
                rects.push((hit, rect, enabled));
                if hit == next {
                    x -= label_w + gap;
                }
            }
        }
        if let Some((_, next_rect, _)) = rects.first() {
            ctx.text_role_in_band(
                &label,
                ui::TextRole::Caption,
                theme.text_dim,
                next_rect.x0 - gap - label_w,
                Rect::new(bounds.x0, y0, bounds.x1, y0 + height),
                None,
            );
        }
        self.collection_page_hits.extend(rects);
    }

    /// The title and the one-line summary of the folder import in flight or
    /// just finished, if any.
    fn directory_import_summary(&self) -> Option<(String, String, usize, bool)> {
        let count = |n: usize| ArgValue::Number(n as f64);
        if let Some(job) = self.directory_import.as_ref() {
            let title = if job.mode == DirectoryImportMode::Import {
                tr("photos-import-running")
            } else {
                tr("photos-reindex-running")
            };
            let summary = tr_args(
                "photos-import-progress",
                &[
                    ("imported", count(job.imported)),
                    ("found", count(job.discovered)),
                    ("issues", count(job.failures.len())),
                ],
            );
            return Some((title, summary, job.failures.len(), true));
        }
        let receipt = self.directory_import_receipt.as_ref()?;
        let title = match (receipt.cancelled, receipt.mode) {
            (true, _) => tr("photos-import-stopped"),
            (false, DirectoryImportMode::Reindex) => tr("photos-reindex-complete"),
            (false, DirectoryImportMode::Import) => tr("photos-import-complete"),
        };
        // The facts that happened, as sentences; a count of zero says nothing.
        let mut sentences = vec![tr_args(
            "photos-import-result",
            &[
                ("imported", count(receipt.imported)),
                ("found", count(receipt.discovered)),
            ],
        )];
        if receipt.refreshed + receipt.unchanged > 0 {
            sentences.push(tr_args(
                "photos-import-result-kept",
                &[
                    ("refreshed", count(receipt.refreshed)),
                    ("unchanged", count(receipt.unchanged)),
                ],
            ));
        }
        if receipt.failure_count > 0 {
            sentences.push(tr_count(
                "photos-import-result-issues",
                receipt.failure_count,
            ));
        }
        if receipt.limit_reached {
            sentences.push(tr("photos-import-limit"));
        }
        Some((title, sentences.join(" "), receipt.failure_count, false))
    }

    /// The folder import's progress or result, on the floating material at
    /// the bottom-trailing corner of the content, with its actions. While its
    /// issues are open in their dialog, the dialog stands for it.
    fn draw_directory_import_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some((title, summary, issue_count, active)) = self
            .directory_import_summary()
            .filter(|_| !self.directory_import_details_open)
        else {
            for (_, button) in &mut self.directory_import_buttons {
                button.set_enabled(false);
                button.apply_layout_external(Rect::ZERO);
            }
            return;
        };
        let spacing = &theme.tokens.spacing;
        let pad = f64::from(spacing.s4);
        let gap = f64::from(spacing.s2);
        let outer = f64::from(spacing.s4);
        let width = (self.canvas_rect.width() - 2.0 * outer)
            .min(f64::from(theme.tokens.metrics.panel_width_list) * 1.2);
        let inner_w = width - 2.0 * pad;
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, &ink);
        let title_style = ink.text(ui::TextRole::Body).strong();
        let title_h = f64::from(
            ctx.text
                .measure_style(&title, Some(inner_w as f32), &title_style)
                .1,
        );
        let summary_h = ctx
            .measure_role(&summary, ui::TextRole::Caption, Some(inner_w as f32))
            .height;
        let progress_h = if active {
            f64::from(theme.tokens.radii.detail) * 2.0 + gap
        } else {
            0.0
        };
        let button_h = ui::widget::ControlSize::Regular.height(theme);
        let height =
            pad + title_h + f64::from(spacing.s1) + summary_h + progress_h + gap + button_h + pad;
        let panel = Rect::new(
            self.canvas_rect.x1 - outer - width,
            self.canvas_rect.y1 - outer - height,
            self.canvas_rect.x1 - outer,
            self.canvas_rect.y1 - outer,
        );
        ui::widget::floating_surface::paint(ctx.scene, transform, panel, theme);
        let mut y = panel.y0 + pad;
        ctx.text(
            &title,
            &title_style,
            ink.text_primary,
            Point::new(panel.x0 + pad, y),
            Some(inner_w as f32),
        );
        y += title_h + f64::from(spacing.s1);
        ctx.text_role(
            &summary,
            ui::TextRole::Caption,
            ink.text_dim,
            Point::new(panel.x0 + pad, y),
            Some(inner_w as f32),
        );
        y += summary_h + gap;
        if active {
            let bar = Rect::new(
                panel.x0 + pad,
                y,
                panel.x1 - pad,
                y + f64::from(theme.tokens.radii.detail) * 2.0,
            );
            self.directory_import_progress.apply_layout_external(bar);
            self.directory_import_progress.draw(
                ctx.scene,
                transform,
                ctx.text,
                &ink,
                &PresenceCtx::empty(),
            );
        }
        let visible = |hit| match hit {
            DirectoryStatusHit::Stop => active,
            DirectoryStatusHit::Details => issue_count > 0,
            DirectoryStatusHit::Dismiss => !active,
        };
        let mut x = panel.x1 - pad;
        let y0 = panel.y1 - pad - button_h;
        for (hit, button) in self.directory_import_buttons.iter_mut().rev() {
            if visible(*hit) {
                let label = directory_status_label(*hit);
                let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(ctx.text, &ink);
                let rect = Rect::new(x - width, y0, x, y0 + button_h);
                x = rect.x0 - gap;
                button.set_enabled(true);
                button.apply_layout_external(rect);
                button.draw(ctx.scene, transform, ctx.text, &ink, &PresenceCtx::empty());
            } else {
                button.set_enabled(false);
                button.apply_layout_external(Rect::ZERO);
            }
        }
    }

    /// The folder import's issues in a modal sheet over the window, the list
    /// scrolling in the sheet's body. Painted in the top layer.
    #[expect(
        clippy::too_many_lines,
        reason = "the sheet measures, lays out and paints its rows and actions in one pass"
    )]
    fn draw_directory_import_details(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.directory_import_details_open {
            self.directory_import_details_layout = None;
            return;
        }
        let (failures, failure_count) = if let Some(job) = self.directory_import.as_ref() {
            (job.failures.clone(), job.failures.len())
        } else if let Some(receipt) = self.directory_import_receipt.as_ref() {
            (receipt.failures.clone(), receipt.failure_count)
        } else {
            self.directory_import_details_open = false;
            self.directory_import_details_layout = None;
            return;
        };
        if failures.is_empty() {
            self.directory_import_details_open = false;
            self.directory_import_details_layout = None;
            return;
        }
        let retained = failures.len().min(DIRECTORY_FAILURE_DETAIL_LIMIT);
        let window = presence.viewport.get().unwrap_or(self.rect);
        let max_width = f64::from(theme.tokens.metrics.panel_width_sheet);
        let body_w = ui::widget::modal_dialog::body_width(window, max_width, theme);
        let rows: Vec<(String, String, String)> = failures
            .iter()
            .take(retained)
            .map(|failure| {
                (
                    failure
                        .source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map_or_else(|| tr("photos-import-unnamed"), str::to_owned),
                    failure.message.clone(),
                    if failure.retryable {
                        tr("photos-import-can-retry")
                    } else {
                        tr("photos-import-needs-change")
                    },
                )
            })
            .collect();
        // When there were more issues than were kept, how many in all.
        let count = |n: usize| ArgValue::Number(n as f64);
        let overview = (failure_count > retained).then(|| {
            tr_args(
                "photos-import-issues-total",
                &[("kept", count(retained)), ("count", count(failure_count))],
            )
        });
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        let overview_h = overview.as_deref().map_or(0.0, |overview| {
            PaintCtx::new(scene, transform, text_renderer, theme)
                .measure_role(overview, ui::TextRole::Caption, Some(body_w as f32))
                .height
                + gap
        });
        let faces: Vec<ListRowFace<'_>> = rows
            .iter()
            .enumerate()
            .map(|(index, (name, message, meta))| {
                let face = ListRowFace::new(name).secondary(message).meta(meta);
                if index + 1 < rows.len() {
                    face.separator(false)
                } else {
                    face
                }
            })
            .collect();
        let list_h: f64 = faces
            .iter()
            .map(|face| face.height(text_renderer, theme))
            .sum();
        let body_h = overview_h + list_h;
        let (close, sheet_title) = (tr("photos-close"), tr("photos-import-issues-title"));
        let actions = [ModalAction::primary(&close)];
        let base_spec = ModalSpec::new(&sheet_title)
            .actions(&actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(true);
        let layout = ui::widget::modal_dialog::layout(window, &base_spec, text_renderer, theme);
        let hovered = |index: usize| {
            layout
                .actions
                .get(index)
                .is_some_and(|rect| rect.contains(self.pointer))
        };
        let actions = [ModalAction::primary(&close).hovered(hovered(0))];
        let spec = ModalSpec::new(&sheet_title)
            .actions(&actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(true)
            .close_hovered(layout.close.is_some_and(|rect| rect.contains(self.pointer)));
        ui::widget::modal_dialog::paint_backdrop(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        let scroll = &mut self.directory_import_details_scroll;
        scroll.set_viewport(layout.body, theme);
        scroll.set_content_size(layout.body.width(), body_h);
        let width = scroll.content_rect().width();
        let visible = scroll.visible_content();
        scroll.paint(scene, transform, &sheet, |scene, content| {
            let mut ctx = PaintCtx::new(scene, content, text_renderer, &sheet);
            if let Some(overview) = overview.as_deref() {
                ctx.text_role(
                    overview,
                    ui::TextRole::Caption,
                    sheet.text_dim,
                    Point::ZERO,
                    Some(width as f32),
                );
            }
            let mut y = overview_h;
            for face in &faces {
                let h = face.height(ctx.text, &sheet);
                let row = Rect::new(0.0, y, width, y + h);
                if row.y1 >= visible.y0 && row.y0 <= visible.y1 {
                    face.paint(&mut ctx, row);
                }
                y += h;
            }
        });
        ui::widget::modal_dialog::paint_actions(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        self.directory_import_details_layout = Some(layout);
    }

    /// The content column inside its gutters: the page edge of every library
    /// surface.
    fn content_frame(&self, theme: &Theme) -> Rect {
        let rhythm = &theme.tokens.spacing.rhythm;
        let side = f64::from(rhythm.group);
        let top = f64::from(rhythm.section);
        let bounds = self.canvas_rect;
        Rect::new(
            bounds.x0 + side,
            bounds.y0 + top,
            (bounds.x1 - side).max(bounds.x0 + side),
            (bounds.y1 - top).max(bounds.y0 + top),
        )
    }

    /// The album overview: one media card per album, its cover the first
    /// photograph in it, in a grid that reflows with the width. Covers come
    /// from the bounded thumbnail cache; originals are never decoded here.
    #[expect(
        clippy::too_many_lines,
        reason = "the album grid measures, scrolls and paints its rows in one pass"
    )]
    fn draw_albums(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        self.places_hits.clear();
        self.album_cards.clear();
        self.album_edit_cards.clear();
        self.album_editor_hits.clear();
        self.collection_page_hits.clear();

        let content = self.content_frame(theme);
        if self.library_albums.is_empty() {
            let title = tr("photos-albums-empty-title");
            let body = tr("photos-albums-empty-body");
            let action = tr("photos-new-album");
            self.album_editor_hits.extend(
                EmptyState::paint(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    self.canvas_rect,
                    &EmptyStateContent {
                        icon: Some(Icon::GalleryVerticalEnd),
                        illustration: None,
                        title: &title,
                        message: Some(&body),
                        action_label: Some(&action),
                        error: false,
                    },
                    self.pointer,
                )
                .map(|rect| (AlbumEditorHit::New, rect)),
            );
            return;
        }

        let rhythm = &theme.tokens.spacing.rhythm;
        let column_gap = f64::from(rhythm.section);
        let row_gap = f64::from(rhythm.group);
        let extent = ui::widget::MediaSize::Regular.extent(theme);
        let columns = ui::widget::card_columns(content.width(), extent, column_gap).max(1);
        let card_w = ui::widget::card_column_width(content.width(), columns, column_gap);
        let metas: Vec<String> = self
            .library_albums
            .iter()
            .map(|(_, _, count)| tr_count("photos-count-photos", *count))
            .collect();
        // Every album is in the grid; the grid scrolls in the column.
        let viewport = Rect::new(
            content.x0,
            self.canvas_rect.y0,
            content.x1,
            self.canvas_rect.y1,
        );
        let mut rows = Vec::new();
        let mut content_h = content.y0 - viewport.y0;
        for (row_index, row) in self.library_albums.chunks(columns).enumerate() {
            let text_h = row
                .iter()
                .enumerate()
                .map(|(column, (_, name, _))| {
                    MediaCardFace::new(ui::widget::MediaArt::Placeholder)
                        .aspect(ui::widget::MediaAspect::Square)
                        .title(name)
                        .meta(
                            metas
                                .get(row_index * columns + column)
                                .map_or("", String::as_str),
                        )
                        .text_height(text_renderer, theme, card_w)
                })
                .fold(0.0, f64::max);
            rows.push((content_h, card_w + text_h));
            content_h += card_w + text_h + row_gap;
        }
        content_h += f64::from(rhythm.section) - row_gap;
        let metrics = ScrollMetrics::new(content_h, viewport.height(), self.albums_scroll);
        self.albums_scroll = self.albums_scroll.clamp(0.0, metrics.max_scroll());
        self.albums_viewport = viewport;
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &Rect::new(
                self.canvas_rect.x0,
                viewport.y0,
                self.canvas_rect.x1,
                viewport.y1,
            ),
        );
        let mut placed = Vec::new();
        for ((top, card_h), (row_index, row)) in rows
            .iter()
            .zip(self.library_albums.chunks(columns).enumerate())
        {
            let y = viewport.y0 + top - self.albums_scroll;
            if y + card_h < viewport.y0 || y > viewport.y1 {
                continue;
            }
            for (column, (id, name, _)) in row.iter().enumerate() {
                let x = content.x0 + column as f64 * (card_w + column_gap);
                let card = Rect::new(x, y, x + card_w, y + card_h);
                let cover = self
                    .library_album_covers
                    .get(id)
                    .and_then(|covers| covers.first())
                    .and_then(|cover| self.thumbnail_for(cover));
                let art = cover.map_or(ui::widget::MediaArt::Placeholder, |image| {
                    ui::widget::MediaArt::Image(image)
                });
                let visible = card.intersect(viewport);
                MediaCardFace::new(art)
                    .aspect(ui::widget::MediaAspect::Square)
                    .title(name)
                    .meta(
                        metas
                            .get(row_index * columns + column)
                            .map_or("", String::as_str),
                    )
                    .interaction(ui::widget::Interaction {
                        hovered: visible.contains(self.pointer),
                        ..Default::default()
                    })
                    .paint(
                        &mut PaintCtx::new(scene, transform, text_renderer, theme),
                        card,
                    );
                placed.push((Some(id.clone()), visible));
            }
        }
        scene.pop_layer();
        self.album_cards.extend(placed);
        if metrics.overflows() {
            ui::widget::scrollbar::Scrollbar::vertical().draw(
                scene,
                transform,
                Self::scroll_track(viewport, theme),
                ScrollMetrics::new(content_h, viewport.height(), self.albums_scroll),
                theme,
                false,
            );
        }
    }

    /// The People surface: who you named and what this device suggests, as
    /// media cards; one person's photographs and decisions in the detail.
    /// It never paints a made-up suggestion when no face model is installed.
    fn draw_people(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        let layout = self.people_layout.clone();
        let frame = self.content_frame(theme);
        // What does not fit stays inside the content column.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.canvas_rect,
        );
        self.paint_people(scene, transform, text_renderer, theme, &layout, frame);
        scene.pop_layer();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the People overview and detail paint from one shared layout"
    )]
    fn paint_people(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &PeopleLayout,
        frame: Rect,
    ) {
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);

        if let Some(selection) = self.people_selection.clone() {
            let (title, summary, photo_ids, private, hidden) = match &selection {
                PeopleSelection::Approved(name) => {
                    let ids = self
                        .people_approved
                        .iter()
                        .find(|person| &person.label == name)
                        .map_or_else(Vec::new, |person| person.photo_ids.clone());
                    (
                        name.clone(),
                        tr_count("photos-people-approved-summary", ids.len()),
                        ids,
                        false,
                        false,
                    )
                }
                PeopleSelection::Suggestion(id) => self.people_controller.cluster(id).map_or_else(
                    || {
                        (
                            tr("photos-people-unavailable-title"),
                            tr("photos-people-unavailable-body"),
                            Vec::new(),
                            true,
                            true,
                        )
                    },
                    |cluster| {
                        (
                            cluster
                                .display_name
                                .clone()
                                .unwrap_or_else(|| tr("photos-people-who")),
                            tr_count("photos-people-suggestion-summary", cluster.photos.len()),
                            cluster.visible_photo_ids(),
                            true,
                            cluster.hidden || cluster.rejected,
                        )
                    },
                ),
            };
            let reveal = if theme.reduce_motion {
                1.0_f32
            } else {
                self.people_detail_transition.value(ui::time::now())
            };
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                reveal,
                transform,
                &self.canvas_rect,
            );
            for (rect, photo_id) in layout.detail_photos.iter().zip(photo_ids.iter()) {
                let art = self.thumbnail_for(photo_id).map_or(
                    ui::widget::MediaArt::Placeholder,
                    ui::widget::MediaArt::Image,
                );
                MediaCardFace::new(art)
                    .aspect(ui::widget::MediaAspect::Square)
                    .paint(&mut ctx, *rect);
            }
            if let Some(at) = layout.detail_title {
                let title = if self.people_merge_mode {
                    if private {
                        tr("photos-people-combine-title")
                    } else {
                        tr("photos-people-merge-title")
                    }
                } else {
                    title
                };
                let width = (frame.width()) as f32;
                let style = theme.text(ui::TextRole::Title);
                let shown = ctx.text.elide_to_width_style(&title, width, &style);
                ctx.text(&shown, &style, theme.text_primary, at, None);
                if private && !self.people_merge_mode {
                    let label = tr("photos-people-only-here");
                    let title_w = f64::from(ctx.text.measure_style(&shown, None, &style).0);
                    let badge_w = ui::widget::BadgeFace::intrinsic_width(&label, ctx.text, theme);
                    let badge_h = ui::widget::BadgeFace::height(theme);
                    let line = Self::line_of(theme, ui::TextRole::Title);
                    let x = at.x + title_w + f64::from(theme.tokens.spacing.rhythm.sibling);
                    let y = at.y + (line - badge_h) / 2.0;
                    if x + badge_w <= frame.x1 {
                        ui::widget::BadgeFace::new(
                            Rect::new(x, y, x + badge_w, y + badge_h),
                            &label,
                        )
                        .paint(ctx.scene, transform, ctx.text, theme);
                    }
                }
            }
            let text = if self.people_merge_mode {
                tr("photos-people-merge-body")
            } else if hidden {
                tr("photos-people-hidden-body")
            } else {
                summary
            };
            ctx.text_role(
                &text,
                ui::TextRole::Body,
                theme.text_dim,
                Point::new(layout.detail_text.x0, layout.detail_text.y0),
                Some(layout.detail_text.width() as f32),
            );
            if let Some(at) = layout.name_label {
                ctx.text_role(
                    &tr("photos-people-name"),
                    ui::TextRole::Label,
                    theme.text_dim,
                    at,
                    None,
                );
            }
            ctx.scene.pop_layer();
            return;
        }

        if let Some(at) = layout.recognition_heading {
            ctx.text_role(
                &tr("photos-people-recognition"),
                ui::TextRole::Heading,
                theme.text_primary,
                at,
                None,
            );
        }
        let scan_state = match self.people_scan_progress.status.as_str() {
            "running" => tr("photos-people-state-running"),
            "error" => self
                .people_scan_progress
                .last_error
                .clone()
                .unwrap_or_else(|| tr("photos-people-state-error")),
            "cancelled" => tr("photos-people-state-paused"),
            _ => match self.people_controller.capability() {
                crate::people::FaceCapability::DisabledByOwner => tr("photos-people-state-off"),
                crate::people::FaceCapability::NoVerifiedFaceModel => {
                    tr("photos-people-state-no-model")
                }
                crate::people::FaceCapability::Ready { .. } => tr("photos-people-state-ready"),
            },
        };
        let text_rect = layout.recognition_text;
        let shown = ctx.text.elide_to_width_style(
            &scan_state,
            text_rect.width() as f32,
            &theme.text(ui::TextRole::Body),
        );
        ctx.text_role(
            &shown,
            ui::TextRole::Body,
            theme.text_dim,
            Point::new(text_rect.x0, text_rect.y0),
            None,
        );

        // The cards scroll under the recognition section.
        let lanes = layout.lanes;
        ctx.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &Rect::new(self.canvas_rect.x0, lanes.y0, self.canvas_rect.x1, lanes.y1),
        );
        for (at, key) in [
            (layout.people_heading, "photos-people"),
            (layout.suggestions_heading, "photos-people-suggestions"),
        ] {
            if let Some(at) = at {
                ctx.text_role(
                    &tr(key),
                    ui::TextRole::Heading,
                    theme.text_primary,
                    at,
                    None,
                );
            }
        }
        for (empty, key) in [
            (layout.people_empty, "photos-people-empty"),
            (layout.suggestions_empty, "photos-people-suggestions-empty"),
        ] {
            if let Some(empty) = empty {
                ctx.text_role(
                    &tr(key),
                    ui::TextRole::Body,
                    theme.text_dim,
                    Point::new(empty.x0, empty.y0),
                    Some(empty.width() as f32),
                );
            }
        }
        let only_here = tr("photos-people-only-here");
        for control in &self.people_controls {
            let (name, count_line, private_line, photo_ids) = match &control.intent {
                PeopleIntent::OpenApproved(name) => {
                    let person = self
                        .people_approved
                        .iter()
                        .find(|person| &person.label == name);
                    (
                        name.clone(),
                        tr_count(
                            "photos-count-photos",
                            person.map_or(0, crate::people::SharedPerson::count),
                        ),
                        None,
                        person.map_or_else(Vec::new, |person| person.photo_ids.clone()),
                    )
                }
                PeopleIntent::OpenSuggestion(id) => {
                    let Some(cluster) = self.people_controller.cluster(id) else {
                        continue;
                    };
                    (
                        cluster
                            .display_name
                            .clone()
                            .unwrap_or_else(|| tr("photos-people-suggested")),
                        tr_count("photos-count-photos", cluster.photos.len()),
                        Some(only_here.as_str()),
                        cluster.visible_photo_ids(),
                    )
                }
                _ => continue,
            };
            if control.bounds.y1 < lanes.y0 || control.bounds.y0 > lanes.y1 {
                continue;
            }
            let art = photo_ids
                .first()
                .and_then(|id| self.thumbnail_for(id))
                .map_or(
                    ui::widget::MediaArt::Placeholder,
                    ui::widget::MediaArt::Image,
                );
            let mut face = MediaCardFace::new(art)
                .aspect(ui::widget::MediaAspect::Square)
                .title(&name)
                .meta(&count_line)
                .interaction(ui::widget::Interaction {
                    hovered: control.bounds.contains(self.pointer) && lanes.contains(self.pointer),
                    focused: control.button.is_focused(),
                    ..Default::default()
                });
            if let Some(line) = private_line {
                face = face.meta(line);
            }
            face.paint(&mut ctx, control.bounds);
        }
        ctx.scene.pop_layer();
        let metrics = ScrollMetrics::new(layout.lanes_content, lanes.height(), self.people_scroll);
        if metrics.overflows() {
            ui::widget::scrollbar::Scrollbar::vertical().draw(
                ctx.scene,
                transform,
                Self::scroll_track(lanes, theme),
                metrics,
                theme,
                false,
            );
        }
    }

    /// The scroll thumb's track beside a scrolling region of the content
    /// column: in the column's trailing gutter, clear of what scrolls.
    fn scroll_track(region: Rect, theme: &Theme) -> Rect {
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let x0 = region.x1 + (f64::from(theme.tokens.spacing.rhythm.group) - gutter) / 2.0;
        Rect::new(x0, region.y0, x0 + gutter, region.y1)
    }

    /// Photo analysis: how much of the library is searchable, the run in
    /// progress and its controls, the local model, and the photographs that
    /// failed. Pixels stay on this device; the compact results are shared.
    #[expect(
        clippy::too_many_lines,
        reason = "the analysis page lays out its stats, run, model and failures in one pass"
    )]
    fn draw_library_analysis(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use crate::library_analysis::LibraryAnalysisPhase;

        self.library_cards.clear();
        self.library_details_cards.clear();
        self.metadata_filter_cards.clear();
        self.places_hits.clear();
        self.library_analysis_failure_hits.clear();

        let frame = self.content_frame(theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let (related, sibling, group) = (
            f64::from(rhythm.related),
            f64::from(rhythm.sibling),
            f64::from(rhythm.group),
        );
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = frame.y0;

        // What is searchable now, ready, and waiting for its bytes.
        let (analyzed, available, awaiting) = self.library_analysis_coverage;
        let values = [
            analyzed.to_string(),
            available.to_string(),
            awaiting.to_string(),
        ];
        let labels = [
            tr("photos-analysis-searchable"),
            tr("photos-analysis-ready"),
            tr("photos-analysis-waiting"),
        ];
        let tiles = [
            StatTileFace::new(&labels[0], &values[0]),
            StatTileFace::new(&labels[1], &values[1]),
            StatTileFace::new(&labels[2], &values[2]),
        ];
        // The tiles reflow as a `StatGroup` does: all in one row where they
        // fit, else one per row, never an uneven 2 + 1.
        let tile_min = (Self::line_of(theme, ui::TextRole::Body) * 7.0).round();
        let fits = ui::widget::card_columns(frame.width(), tile_min, group);
        let columns = (1..=tiles.len().min(fits))
            .rev()
            .find(|columns| tiles.len().is_multiple_of(*columns))
            .unwrap_or(1);
        let tile_w = ui::widget::card_column_width(frame.width(), columns, group);
        let tile_h = tiles
            .iter()
            .map(|tile| tile.height(ctx.text, theme))
            .fold(0.0, f64::max);
        for (index, tile) in tiles.iter().enumerate() {
            let column = index % columns;
            let row = index / columns;
            let x = frame.x0 + column as f64 * (tile_w + group);
            let top = y + row as f64 * (tile_h + sibling);
            tile.paint(&mut ctx, Rect::new(x, top, x + tile_w, top + tile_h));
        }
        y += 3usize.div_ceil(columns) as f64 * (tile_h + sibling) - sibling + group;

        // The run.
        let phase = self.library_analysis.phase;
        let phase_label = match phase {
            LibraryAnalysisPhase::Idle => tr("photos-analysis-idle"),
            LibraryAnalysisPhase::Running => tr("photos-analysis-running"),
            LibraryAnalysisPhase::Paused => tr("photos-analysis-paused"),
            LibraryAnalysisPhase::Complete => tr("photos-analysis-finished"),
        };
        ctx.text_role(
            &phase_label,
            ui::TextRole::Heading,
            theme.text_primary,
            Point::new(frame.x0, y),
            None,
        );
        y += Self::line_of(theme, ui::TextRole::Heading) + f64::from(rhythm.tight);
        // Without a model on this device there is nothing to start; the
        // page says what it needs instead of offering a dead button.
        let has_model = self.library_analysis_capability.is_some();
        let summary = match phase {
            LibraryAnalysisPhase::Idle if has_model => tr("photos-analysis-idle-body"),
            LibraryAnalysisPhase::Idle => tr("photos-analysis-needs-model"),
            _ => tr_args(
                "photos-analysis-progress",
                &[
                    (
                        "done",
                        ArgValue::Number(self.library_analysis.completed as f64),
                    ),
                    (
                        "total",
                        ArgValue::Number(self.library_analysis.total as f64),
                    ),
                    (
                        "failed",
                        ArgValue::Number(self.library_analysis.failed as f64),
                    ),
                ],
            ),
        };
        let summary_h = ctx
            .measure_role(&summary, ui::TextRole::Body, Some(frame.width() as f32))
            .height;
        ctx.text_role(
            &summary,
            ui::TextRole::Body,
            theme.text_dim,
            Point::new(frame.x0, y),
            Some(frame.width() as f32),
        );
        y += summary_h + sibling;
        if phase == LibraryAnalysisPhase::Idle {
            self.library_analysis_progress
                .apply_layout_external(Rect::ZERO);
        } else {
            self.library_analysis_progress
                .set_progress(self.library_analysis.progress() as f32);
            let bar_h = f64::from(theme.tokens.radii.detail) * 2.0;
            self.library_analysis_progress
                .apply_layout_external(Rect::new(frame.x0, y, frame.x1, y + bar_h));
            self.library_analysis_progress
                .draw(ctx.scene, transform, ctx.text, theme, presence);
            y += bar_h + related;
            if let Some(name) = self.library_analysis.current_name.as_deref() {
                ctx.text_role(
                    name,
                    ui::TextRole::Caption,
                    theme.text_dim,
                    Point::new(frame.x0, y),
                    Some(frame.width() as f32),
                );
                y += Self::line_of(theme, ui::TextRole::Caption);
            }
            y += related;
        }
        let visible_hit = match phase {
            LibraryAnalysisPhase::Running => LibraryAnalysisHit::Pause,
            LibraryAnalysisPhase::Paused => LibraryAnalysisHit::Resume,
            _ => LibraryAnalysisHit::Start,
        };
        let button_h = ui::widget::ControlSize::Regular.height(theme);
        let mut x = frame.x0;
        for (hit, button) in &mut self.library_analysis_buttons {
            let visible = (*hit == visible_hit && (has_model || *hit != LibraryAnalysisHit::Start))
                || (*hit == LibraryAnalysisHit::RetryFailures
                    && self.library_analysis.has_failures()
                    && phase != LibraryAnalysisPhase::Running);
            button.set_enabled(match *hit {
                LibraryAnalysisHit::Start => {
                    self.library_analysis_capability.is_some() && available > 0
                }
                LibraryAnalysisHit::Pause => phase == LibraryAnalysisPhase::Running,
                LibraryAnalysisHit::Resume => phase == LibraryAnalysisPhase::Paused,
                LibraryAnalysisHit::RetryFailures => self.library_analysis.has_failures(),
            });
            if visible {
                let label = tr(match hit {
                    LibraryAnalysisHit::Start => "photos-analysis-start",
                    LibraryAnalysisHit::Pause => "photos-analysis-pause",
                    LibraryAnalysisHit::Resume => "photos-analysis-resume",
                    LibraryAnalysisHit::RetryFailures => "photos-analysis-retry",
                });
                let icon = match hit {
                    LibraryAnalysisHit::Start => Icon::Sparkles,
                    LibraryAnalysisHit::Pause => Icon::Pause,
                    LibraryAnalysisHit::Resume => Icon::Play,
                    LibraryAnalysisHit::RetryFailures => Icon::Refresh,
                };
                let width = ButtonFace::new(Rect::ZERO, &label)
                    .icon(icon)
                    .intrinsic_width(ctx.text, theme);
                button.apply_layout_external(Rect::new(x, y, x + width, y + button_h));
                button.draw(ctx.scene, transform, ctx.text, theme, presence);
                x += width + related;
            } else {
                button.apply_layout_external(Rect::ZERO);
            }
        }
        if x > frame.x0 {
            y += button_h + group;
        }

        // The model that does the work.
        let model_rows: Vec<(String, String, bool)> = self
            .library_analysis_capability
            .as_ref()
            .map(|capability| {
                vec![
                    (
                        tr("photos-analysis-model"),
                        capability.model_label.clone(),
                        false,
                    ),
                    (
                        tr("photos-analysis-revision"),
                        capability.revision.chars().take(10).collect(),
                        true,
                    ),
                ]
            })
            .unwrap_or_default();
        let rows: Vec<PropertyRow<'_>> = model_rows
            .iter()
            .map(|(label, value, identifier)| {
                let row = PropertyRow::new(label, value);
                if *identifier {
                    row.with_kind(ui::TextKind::Identifier)
                } else {
                    row
                }
            })
            .collect();
        let list = PropertyListFace::new(&rows);
        let list_w = frame
            .width()
            .min(f64::from(theme.tokens.metrics.panel_width_sheet));
        let list_h = list.height(ctx.text, theme, list_w);
        if !rows.is_empty() && y + list_h <= frame.y1 {
            list.paint(
                &mut ctx,
                Rect::new(frame.x0, y, frame.x0 + list_w, y + list_h),
            );
        }
        y += list_h + group;

        // Photographs the model could not read.
        if self.library_analysis.has_failures() && y < frame.y1 {
            ctx.text_role(
                &tr("photos-analysis-needs-attention"),
                ui::TextRole::Heading,
                theme.text_primary,
                Point::new(frame.x0, y),
                None,
            );
            y += Self::line_of(theme, ui::TextRole::Heading) + related;
            let failures: Vec<_> = self
                .library_analysis
                .failures()
                .rev()
                .take(4)
                .map(|failure| {
                    (
                        failure.photo_id.clone(),
                        failure.name.clone(),
                        failure.message.clone(),
                    )
                })
                .collect();
            for (index, (photo_id, name, message)) in failures.iter().enumerate() {
                let mut face = ListRowFace::new(name)
                    .secondary(message)
                    .status(ui::widget::RowStatus::Error)
                    .disclosure();
                if index + 1 < failures.len() {
                    face = face.separator(false);
                }
                let h = face.height(ctx.text, theme);
                if y + h > frame.y1 {
                    break;
                }
                let row = Rect::new(frame.x0, y, frame.x1, y + h);
                let face = face.interaction(ui::widget::Interaction {
                    hovered: row.contains(self.pointer),
                    ..Default::default()
                });
                face.paint(&mut ctx, row);
                self.library_analysis_failure_hits
                    .push((photo_id.clone(), row));
                y += h;
            }
        }
    }

    /// The timeline: the photographs of each capture day in justified rows
    /// at their own aspect ratios under the day's heading, scrolled by the
    /// date index at the trailing edge. Only the rows in view are
    /// materialised; the rest stay reachable through search and resources.
    #[expect(
        clippy::too_many_lines,
        reason = "the timeline lays out, paints and records the hit targets of the rows in view in one pass"
    )]
    fn draw_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.album_cards.clear();
        self.metadata_filter_cards.clear();
        self.library_page_hits.clear();
        self.library_details_cards.clear();
        self.places_hits.clear();
        self.collection_page_hits.clear();
        if self.library_items.is_empty() {
            let (title, body) = Self::library_empty_state_text();
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
            return;
        }
        let frame = self.content_frame(theme);
        let index_w = ui::widget::date_indexed_scrollbar::DEFAULT_TRACK_WIDTH;
        let top = frame.y0;
        let viewport_height = (self.canvas_rect.y1 - top).max(0.0);
        self.timeline_viewport_height = viewport_height;
        let timeline_left = frame.x0;
        let geometry = self.timeline_geometry(theme);
        let timeline_width = geometry.width;
        let anchor = self.timeline_projection.anchor_at(self.timeline_scroll_y);
        let geometry_changed = self.timeline_projection.set_geometry(geometry);
        if geometry_changed
            && let Some(anchor) = anchor.as_ref()
            && let Some(restored) = self
                .timeline_projection
                .restore_anchor(anchor, viewport_height)
        {
            self.timeline_scroll_y = restored;
        }
        self.timeline_scroll_y = self
            .timeline_projection
            .clamp_scroll(self.timeline_scroll_y, viewport_height);
        self.timeline_index_metrics = ScrollMetrics::new(
            self.timeline_projection.total_height(),
            viewport_height,
            self.timeline_scroll_y,
        );
        self.timeline_index_markers = self
            .timeline_projection
            .date_markers(viewport_height)
            .into_iter()
            .map(|(day, position, major)| {
                DateIndexMarker::new(
                    day.clone(),
                    timeline_date_label(&day),
                    position,
                    if major {
                        DateIndexMarkerKind::Major
                    } else {
                        DateIndexMarkerKind::Minor
                    },
                )
            })
            .collect();
        self.timeline_index
            .set_rect(Rect::new(frame.x1 - index_w, top, frame.x1, frame.y1));
        let visible_days = self
            .timeline_projection
            .visible_days(self.timeline_scroll_y, viewport_height);
        let photo_images: HashMap<_, _> = self
            .observed_content
            .iter()
            .flat_map(|content| content.library.photos.values())
            .map(|photo| (photo.id.as_str(), &photo.image))
            .collect();
        let visible_items: Vec<_> = self
            .timeline_projection
            .visible_photos(self.timeline_scroll_y, viewport_height)
            .into_iter()
            .filter_map(|entry| {
                let image = photo_images.get(entry.photo.id.as_str())?;
                Some((
                    entry.photo.id.clone(),
                    thumbnail_key(image),
                    entry.photo.favorite,
                    Rect::new(
                        timeline_left + entry.x,
                        top + entry.y,
                        timeline_left + entry.x + entry.width,
                        top + entry.y + entry.height,
                    ),
                ))
            })
            .collect();
        // The moving content is clipped to its region, so a fast scroll
        // never paints over the header row or the status bar.
        let clip = Rect::new(
            self.canvas_rect.x0,
            top,
            self.canvas_rect.x1,
            self.canvas_rect.y1,
        );
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &clip,
        );
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        for day in visible_days {
            let heading_y = top + day.y;
            ctx.text_role(
                &timeline_date_label(&day.key),
                ui::TextRole::Subheading,
                theme.text_primary,
                Point::new(timeline_left, heading_y),
                Some(timeline_width as f32),
            );
        }
        let picking = self.selection_mode || self.album_editor;
        for (id, key, favorite, card) in &visible_items {
            let image = self.library_thumbnails.get(key);
            let art = image.map_or(
                ui::widget::MediaArt::Placeholder,
                ui::widget::MediaArt::Image,
            );
            let hovered = card.contains(self.pointer) && clip.contains(self.pointer);
            let chosen = (self.selection_mode && self.selected_photo_ids.contains(id))
                || (self.album_editor && self.album_editor_members.contains(id));
            MediaCardFace::new(art)
                .aspect(ui::widget::MediaAspect::Ratio(
                    card.width() / card.height().max(1.0),
                ))
                .interaction(ui::widget::Interaction {
                    hovered: hovered && !picking,
                    ..Default::default()
                })
                .paint(&mut ctx, *card);
            let fully_visible = card.y0 >= top && card.y1 <= self.canvas_rect.y1;
            if picking {
                Self::paint_pick_mark(&mut ctx, *card, chosen, hovered);
            } else if hovered && fully_visible {
                self.paint_card_actions(&mut ctx, id, *card, *favorite);
            }
            if fully_visible {
                if !picking {
                    let actions = card_action_controls(*card, theme);
                    self.library_details_cards
                        .push((id.clone(), actions.details));
                }
                self.library_cards.push((id.clone(), *card, *favorite));
            }
        }
        ctx.scene.pop_layer();
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
    }

    /// The timeline's row geometry in the content column under `theme`: the
    /// width beside the date index, the row height for the density, the
    /// tight gap between photographs and the height of a day's heading.
    fn timeline_geometry(&self, theme: &Theme) -> TimelineGeometry {
        let frame = self.content_frame(theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let index_w = ui::widget::date_indexed_scrollbar::DEFAULT_TRACK_WIDTH;
        TimelineGeometry {
            width: (frame.width() - index_w - f64::from(rhythm.related)).max(1.0),
            target_height: timeline_row_height(theme, self.timeline_density),
            gap: f64::from(rhythm.tight),
            header_height: Self::line_of(theme, ui::TextRole::Subheading)
                + f64::from(rhythm.related),
            day_gap: f64::from(rhythm.group),
        }
    }

    /// While photographs are being chosen, each shows whether it is in:
    /// a check on the inverted fill when chosen, an empty ring otherwise.
    fn paint_pick_mark(ctx: &mut PaintCtx<'_>, card: Rect, chosen: bool, hovered: bool) {
        use vello::peniko::Fill;
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let inset = f64::from(theme.tokens.spacing.s2);
        let side = f64::from(theme.tokens.metrics.icon_lg);
        let mark = Rect::new(
            card.x1 - inset - side,
            card.y0 + inset,
            card.x1 - inset,
            card.y0 + inset + side,
        );
        let disc = Circle::new(mark.center(), side / 2.0);
        if chosen {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.intent.inverted,
                None,
                &disc,
            );
            Icon::Check.draw(
                ctx.scene,
                ctx.transform,
                mark.inset(-f64::from(theme.tokens.spacing.s1)),
                colors.intent.on_inverted,
            );
        } else {
            ui::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &disc,
                ui::floating_surface::Material::Floating,
                theme,
            );
            if hovered {
                ui::widget::paint_focus_ring(ctx.scene, ctx.transform, mark, side / 2.0, theme);
            }
        }
    }

    /// The hover actions of a photograph: details at the leading corner,
    /// favourite and remove at the trailing one, each on the floating
    /// material over the picture.
    fn paint_card_actions(&self, ctx: &mut PaintCtx<'_>, id: &str, card: Rect, favorite: bool) {
        let theme = ctx.theme;
        let actions = card_action_controls(card, theme);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let confirming = self.library_remove_confirm.as_deref() == Some(id);
        let radius = f64::from(theme.tokens.radii.control);
        for group in [actions.details, actions.remove.union(actions.favorite)] {
            ui::floating_surface::paint_material(
                ctx.scene,
                ctx.transform,
                &RoundedRect::from_rect(group, radius),
                ui::floating_surface::Material::Floating,
                theme,
            );
        }
        let buttons = [
            (
                actions.details,
                Icon::Pencil,
                tr("photos-edit-details"),
                ButtonRole::Plain,
            ),
            (
                actions.remove,
                if confirming {
                    Icon::Check
                } else {
                    Icon::Trash2
                },
                if confirming {
                    tr("photos-confirm-remove")
                } else {
                    tr("photos-remove")
                },
                if confirming {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Plain
                },
            ),
            (
                actions.favorite,
                Icon::Star,
                if favorite {
                    tr("photos-unfavorite")
                } else {
                    tr("photos-favorite")
                },
                ButtonRole::Plain,
            ),
        ];
        for (rect, icon, name, role) in &buttons {
            let face = ButtonFace::icon_only(*rect, *icon, name).role(*role);
            let hovered = face.contains(&ink, self.pointer);
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                &ink,
                &face.hovered(hovered),
            );
        }
    }

    /// The places of the library on an offline map, with the list of places
    /// beside it on a wide window, or the located photographs as a list. The
    /// map is a bundled low-detail land silhouette, never a tile service:
    /// opening Places sends no coordinate off this device.
    #[expect(
        clippy::too_many_lines,
        reason = "the map, its markers and their hit targets share one geometry pass"
    )]
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
        self.collection_page_hits.clear();
        let place_clusters =
            crate::places::clusters(self.places_items.iter().map(|item| &item.photo));
        if self.places_items.is_empty() {
            let title = tr("photos-places-empty-title");
            let body = tr("photos-places-empty-body");
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                self.canvas_rect,
                &EmptyStateContent {
                    icon: Some(Icon::Globe),
                    illustration: None,
                    title: &title,
                    message: Some(&body),
                    action_label: None,
                    error: false,
                },
                self.pointer,
            );
            return;
        }
        if self.places_list_mode {
            let frame = self.content_frame(theme);
            self.draw_places_list(scene, transform, text_renderer, theme, frame);
            return;
        }
        let bounds = self.canvas_rect;
        let show_rail = self.places_open_key.is_none()
            && bounds.width() >= f64::from(theme.tokens.metrics.compact_breakpoint);
        let rail_w = if show_rail {
            f64::from(theme.tokens.metrics.inspector_width)
        } else {
            0.0
        };
        let map = Rect::new(bounds.x0, bounds.y0, bounds.x1 - rail_w, bounds.y1);
        self.places_map_bounds = map;
        // Arriving at Places frames every place on the map, clear of its
        // edges by a marker and its label.
        if std::mem::take(&mut self.places_fit_pending) {
            let points: Vec<(f64, f64)> = place_clusters
                .iter()
                .map(|cluster| (cluster.latitude, cluster.longitude))
                .collect();
            let margin = 3.0 * f64::from(theme.tokens.metrics.min_pointer_target);
            self.places_viewport =
                crate::places::MapViewport::fitting(&points, map.width(), map.height(), margin);
        }

        // The map is content: water on the raised well, land one step off it,
        // coasts derived from the land.
        let colors = &theme.tokens.colors;
        let water = colors.surfaces.raised;
        let land = colors.lift_on(water, 1.0);
        scene.fill(Fill::NonZero, transform, water, None, &map);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &map,
        );
        for landmass in OFFLINE_WORLD_LANDMASSES {
            let mut coast = BezPath::new();
            let mut visible_points = 0_usize;
            for &(latitude, longitude) in *landmass {
                let location = crate::model::PhotoLocation {
                    latitude,
                    longitude,
                    label: None,
                    provenance: crate::model::LocationProvenance::default(),
                };
                let Some((x, y)) =
                    self.places_viewport
                        .project(&location, map.width(), map.height())
                else {
                    continue;
                };
                let point = (map.x0 + x, map.y0 + y);
                if visible_points == 0 {
                    coast.move_to(point);
                } else {
                    coast.line_to(point);
                }
                visible_points += 1;
            }
            if visible_points >= 3 {
                coast.close_path();
                scene.fill(Fill::NonZero, transform, land, None, &coast);
                scene.stroke(
                    &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
                    transform,
                    surface::edge_on(land),
                    None,
                    &coast,
                );
            }
        }
        let camera_feedback = self.places_camera_feedback();
        let marker_r = f64::from(theme.tokens.metrics.min_pointer_target) * 0.75;
        let ring = f64::from(theme.tokens.spacing.s1) * 0.5;
        for cluster in &place_clusters {
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
            let radius = marker_r * (1.0 + camera_feedback * 0.10);
            // A place is its photograph, ringed in the ground so it reads
            // against land and water alike.
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.content,
                None,
                &Circle::new(point, radius),
            );
            let inner = Circle::new(point, radius - ring * 2.0);
            if let Some(image) = cluster
                .photo_ids
                .first()
                .and_then(|photo_id| self.thumbnail_for(photo_id))
            {
                let side = inner.radius * 2.0;
                let scale = (side / f64::from(image.width)).max(side / f64::from(image.height));
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &inner,
                );
                scene.draw_image(
                    image,
                    transform
                        * Affine::translate((
                            point.x - f64::from(image.width) * scale * 0.5,
                            point.y - f64::from(image.height) * scale * 0.5,
                        ))
                        * Affine::scale(scale),
                );
                scene.pop_layer();
            } else {
                scene.fill(Fill::NonZero, transform, land, None, &inner);
            }
            let count = cluster.count().to_string();
            let badge_w = ui::widget::BadgeFace::intrinsic_width(&count, text_renderer, theme);
            let badge_h = ui::widget::BadgeFace::height(theme);
            let badge = Rect::new(
                point.x + radius * 0.5,
                point.y - radius - badge_h * 0.25,
                point.x + radius * 0.5 + badge_w,
                point.y - radius + badge_h * 0.75,
            );
            ui::widget::BadgeFace::new(badge, &count).paint(scene, transform, text_renderer, theme);
            let hit = Rect::new(
                point.x - radius,
                point.y - radius,
                point.x + radius,
                point.y + radius,
            );
            self.places_hits
                .push((PlacesHit::Place(cluster.key.clone()), hit));
            if !show_rail || map.width() >= f64::from(theme.tokens.metrics.panel_width_sheet) {
                let label_w =
                    ui::widget::BadgeFace::intrinsic_width(&cluster.label, text_renderer, theme);
                let label_x =
                    (point.x - label_w * 0.5).clamp(map.x0, (map.x1 - label_w).max(map.x0));
                let label_y = point.y + radius + f64::from(theme.tokens.spacing.s1);
                ui::widget::BadgeFace::new(
                    Rect::new(label_x, label_y, label_x + label_w, label_y + badge_h),
                    &cluster.label,
                )
                .paint(scene, transform, text_renderer, theme);
            }
        }
        scene.pop_layer();
        if show_rail {
            let rail = Rect::new(map.x1, bounds.y0, bounds.x1, bounds.y1);
            self.draw_places_overview_rail(
                scene,
                transform,
                text_renderer,
                theme,
                rail,
                &place_clusters,
            );
        }
    }

    /// The places beside the map, one row each, so the next decision is a
    /// place to open rather than a marker to find.
    fn draw_places_overview_rail(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        clusters: &[crate::places::PlaceCluster],
    ) {
        let inset = f64::from(theme.tokens.spacing.rhythm.section);
        let frame = rail.inset(-inset);
        let counts: Vec<String> = clusters
            .iter()
            .map(|cluster| tr_count("photos-count-photos", cluster.count()))
            .collect();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = frame.y0;
        for (index, (cluster, count)) in clusters.iter().zip(&counts).enumerate() {
            let art = cluster
                .photo_ids
                .first()
                .and_then(|id| self.thumbnail_for(id));
            let mut face = ListRowFace::new(&cluster.label)
                .secondary(count)
                .disclosure();
            face = match art {
                Some(image) => face.leading(ui::widget::RowLeading::Avatar(
                    ui::widget::AvatarFace::new(&cluster.label, &cluster.key)
                        .square()
                        .image(Some(image)),
                )),
                None => face.leading(ui::widget::RowLeading::Icon(Icon::Globe)),
            };
            if index + 1 < clusters.len() {
                face = face.separator(false);
            }
            let h = face.height(ctx.text, theme);
            if y + h > frame.y1 {
                break;
            }
            let row = Rect::new(frame.x0, y, frame.x1, y + h);
            let face = face.interaction(ui::widget::Interaction {
                hovered: row.contains(self.pointer),
                ..Default::default()
            });
            face.paint(&mut ctx, row);
            self.places_hits
                .push((PlacesHit::Place(cluster.key.clone()), row));
            y += h;
        }
    }

    /// The located photographs as a list, a page at a time.
    fn draw_places_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        frame: Rect,
    ) {
        let items: Vec<PlaceItem> = self
            .places_items
            .iter()
            .filter(|item| {
                self.places_open_key.as_ref().is_none_or(|key| {
                    item.photo
                        .location
                        .as_ref()
                        .is_some_and(|location| location.place_key() == *key)
                })
            })
            .cloned()
            .collect();
        let rows: Vec<(String, String, String, String)> = items
            .iter()
            .skip(self.places_page * COLLECTION_PAGE_SIZE)
            .take(COLLECTION_PAGE_SIZE)
            .map(|item| {
                (
                    item.photo.id.clone(),
                    item.photo.image.name.clone(),
                    item.photo
                        .location
                        .as_ref()
                        .map_or_else(String::new, crate::model::PhotoLocation::display_name),
                    item.photo
                        .captured_at
                        .as_deref()
                        .map(timeline_date_label)
                        .unwrap_or_default(),
                )
            })
            .collect();
        let pager_h = ui::widget::ControlSize::Regular.height(theme);
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let mut y = frame.y0;
        for (index, (id, name, place, date)) in rows.iter().enumerate() {
            let mut face = ListRowFace::new(name).secondary(place).meta(date);
            if let Some(image) = self.thumbnail_for(id) {
                face = face.leading(ui::widget::RowLeading::Avatar(
                    ui::widget::AvatarFace::new(name, id)
                        .square()
                        .image(Some(image)),
                ));
            }
            if index + 1 < rows.len() {
                face = face.separator(false);
            }
            let h = face.height(ctx.text, theme);
            if y + h > frame.y1 - pager_h {
                break;
            }
            let row = Rect::new(frame.x0, y, frame.x1, y + h);
            let face = face.interaction(ui::widget::Interaction {
                hovered: row.contains(self.pointer),
                ..Default::default()
            });
            face.paint(&mut ctx, row);
            self.places_hits.push((PlacesHit::Photo(id.clone()), row));
            y += h;
        }
        self.draw_collection_pager(
            &mut ctx,
            Rect::new(frame.x0, frame.y1 - pager_h, frame.x1, frame.y1),
            self.places_page,
            items.len(),
            CollectionPageHit::PlacesPrevious,
            CollectionPageHit::PlacesNext,
        );
    }

    /// The album picker for a selection: a modal sheet over the window, one
    /// row per album, the chosen one lifted. Painted in the top layer.
    #[expect(
        clippy::too_many_lines,
        reason = "the sheet measures, lays out and paints its rows and paged actions in one pass"
    )]
    fn draw_selection_destination_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.selection_destination_hits.clear();
        self.selection_hits.clear();
        if !self.library_mode || !self.selection_album_chooser {
            return;
        }
        let apply_enabled = self.selection_destination_album.is_some();
        let window = presence.viewport.get().unwrap_or(self.rect);
        let max_width = f64::from(theme.tokens.metrics.panel_width_sheet);

        let visible_albums: Vec<(String, String, String)> = self
            .library_albums
            .iter()
            .skip(self.selection_album_page * SELECTION_ALBUMS_PER_PAGE)
            .take(SELECTION_ALBUMS_PER_PAGE)
            .map(|(id, name, count)| {
                (
                    id.clone(),
                    name.clone(),
                    tr_count("photos-count-photos", *count),
                )
            })
            .collect();
        let faces: Vec<ListRowFace<'_>> = visible_albums
            .iter()
            .enumerate()
            .map(|(index, (id, name, count))| {
                let chosen = self.selection_destination_album.as_deref() == Some(id.as_str());
                let next_chosen = visible_albums.get(index + 1).is_some_and(|(next, _, _)| {
                    self.selection_destination_album.as_deref() == Some(next.as_str())
                });
                let face = ListRowFace::new(name)
                    .leading(ui::widget::RowLeading::Icon(Icon::GalleryVerticalEnd))
                    .meta(count)
                    .selected(chosen);
                if index + 1 < visible_albums.len() {
                    face.separator(next_chosen)
                } else {
                    face
                }
            })
            .collect();
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        let heights: Vec<f64> = faces
            .iter()
            .map(|face| face.height(text_renderer, &sheet))
            .collect();
        let body_h: f64 = heights.iter().sum();
        let (cancel, choose, sheet_title) = (
            tr("photos-cancel"),
            tr("photos-choose-album"),
            tr("photos-add-to-album"),
        );
        let previous = tr("photos-a11y-previous-albums");
        let next = tr("photos-a11y-more-albums");
        let base_actions = [
            ModalAction::secondary(&previous).enabled(self.selection_album_page > 0),
            ModalAction::secondary(&next).enabled(
                (self.selection_album_page + 1) * SELECTION_ALBUMS_PER_PAGE
                    < self.library_albums.len(),
            ),
            ModalAction::secondary(&cancel),
            ModalAction::primary(&choose).enabled(apply_enabled),
        ];
        let base_spec = ModalSpec::new(&sheet_title)
            .actions(&base_actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(false);
        let layout = ui::widget::modal_dialog::layout(window, &base_spec, text_renderer, theme);
        let actions: Vec<ModalAction<'_>> = base_actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                action.hovered(
                    layout
                        .actions
                        .get(index)
                        .is_some_and(|rect| rect.contains(self.pointer)),
                )
            })
            .collect();
        let spec = ModalSpec::new(&sheet_title)
            .actions(&actions)
            .body_height(body_h)
            .max_width(max_width)
            .dismiss_on_scrim(false)
            .close_hovered(layout.close.is_some_and(|rect| rect.contains(self.pointer)));
        ui::widget::modal_dialog::paint_backdrop(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        let scroll = &mut self.selection_album_scroll;
        scroll.set_viewport(layout.body, theme);
        scroll.set_content_size(layout.body.width(), body_h);
        let shown = scroll.content_rect();
        let visible = scroll.visible_content();
        let to_window = Vec2::new(shown.x0, shown.y0) - scroll.offset();
        let pointer = shown
            .contains(self.pointer)
            .then(|| scroll.to_content(self.pointer));
        let hits = &mut self.selection_destination_hits;
        scroll.paint(scene, transform, &sheet, |scene, content| {
            let mut ctx = PaintCtx::new(scene, content, text_renderer, &sheet);
            let mut y = 0.0;
            for ((face, h), (id, _, _)) in faces.iter().zip(&heights).zip(&visible_albums) {
                let row = Rect::new(0.0, y, shown.width(), y + h);
                if row.y1 >= visible.y0 && row.y0 <= visible.y1 {
                    let face = face.clone().interaction(ui::widget::Interaction {
                        hovered: pointer.is_some_and(|point| row.contains(point)),
                        ..Default::default()
                    });
                    face.paint(&mut ctx, row);
                }
                hits.push((id.clone(), row + to_window));
                y += h;
            }
        });
        ui::widget::modal_dialog::paint_actions(
            scene,
            transform,
            text_renderer,
            theme,
            &layout,
            &spec,
        );
        let mut rects = layout.actions.iter().copied();
        self.selection_hits.push((
            SelectionHit::PreviousDestinationPage,
            rects.next().unwrap_or(Rect::ZERO),
        ));
        self.selection_hits.push((
            SelectionHit::NextDestinationPage,
            rects.next().unwrap_or(Rect::ZERO),
        ));
        let cancel_rect = rects.next().unwrap_or(Rect::ZERO);
        let apply_rect = rects.next().unwrap_or(Rect::ZERO);
        self.selection_hits
            .push((SelectionHit::CancelDestinationChooser, cancel_rect));
        if let Some(close) = layout.close {
            self.selection_hits
                .push((SelectionHit::CloseDestinationChooser, close));
        }
        if apply_enabled {
            self.selection_hits
                .push((SelectionHit::ApplyDestination, apply_rect));
        }
    }

    /// The album editor in the inspector: the album's name, how many photos
    /// are in it (chosen in the timeline beside it), and its actions.
    #[expect(
        clippy::too_many_lines,
        reason = "the editor lays out its field, count and footer actions in one pass"
    )]
    fn draw_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.library_mode || !self.album_editor {
            if self.library_destination != LibraryDestination::Albums {
                self.album_editor_hits.clear();
            }
            self.album_name_input.apply_layout_external(Rect::ZERO);
            return;
        }
        self.album_editor_hits.clear();
        let title = if self.album_editor_creating {
            tr("photos-new-album")
        } else {
            tr("photos-edit-album")
        };
        let Some((frame, local)) =
            self.paint_inspector(scene, transform, text_renderer, theme, &title)
        else {
            self.album_name_input.apply_layout_external(Rect::ZERO);
            return;
        };
        self.album_editor_rect = frame.panel;
        let rhythm = &local.tokens.spacing.rhythm;
        let control_h = ui::widget::ControlSize::Regular.height(&local);
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, &local);
        let body = frame.body;
        let mut y = body.y0;
        ctx.text_role(
            &tr("photos-people-name"),
            ui::TextRole::Label,
            local.text_dim,
            Point::new(body.x0, y),
            None,
        );
        y += Self::line_of(&local, ui::TextRole::Label) + f64::from(rhythm.tight);
        self.album_name_input
            .apply_layout_external(Rect::new(body.x0, y, body.x1, y + control_h));
        self.album_name_input
            .draw(ctx.scene, transform, ctx.text, &local, presence);
        y += control_h + f64::from(rhythm.group);
        let guidance = tr("photos-album-editor-hint");
        let guidance_h = ctx
            .measure_role(&guidance, ui::TextRole::Body, Some(body.width() as f32))
            .height;
        ctx.text_role(
            &guidance,
            ui::TextRole::Body,
            local.text_dim,
            Point::new(body.x0, y),
            Some(body.width() as f32),
        );
        y += guidance_h + f64::from(rhythm.related);
        ctx.text_role(
            &tr_count("photos-album-members", self.album_editor_members.len()),
            ui::TextRole::Caption,
            local.text_dim,
            Point::new(body.x0, y),
            None,
        );

        // Footer: delete leading, cancel and save trailing.
        let footer = frame.footer;
        let gap = f64::from(rhythm.related);
        let save_label = tr("photos-save");
        let cancel_label = tr("photos-cancel");
        let save_w = ButtonFace::new(Rect::ZERO, &save_label).intrinsic_width(ctx.text, &local);
        let cancel_w = ButtonFace::new(Rect::ZERO, &cancel_label).intrinsic_width(ctx.text, &local);
        let save = Rect::new(footer.x1 - save_w, footer.y0, footer.x1, footer.y1);
        let cancel = Rect::new(
            save.x0 - gap - cancel_w,
            footer.y0,
            save.x0 - gap,
            footer.y1,
        );
        let mut buttons = vec![
            (
                AlbumEditorHit::Cancel,
                cancel,
                cancel_label,
                ButtonRole::Secondary,
                self.album_editor_focus == 1,
            ),
            (
                AlbumEditorHit::Save,
                save,
                save_label,
                ButtonRole::Primary,
                self.album_editor_focus == if self.album_editor_creating { 2 } else { 3 },
            ),
        ];
        if !self.album_editor_creating {
            let (hit, delete_label) = if self.album_delete_confirm {
                (
                    AlbumEditorHit::ConfirmDelete,
                    tr("photos-confirm-delete-album"),
                )
            } else {
                (AlbumEditorHit::Delete, tr("photos-delete-album"))
            };
            let width =
                ButtonFace::new(Rect::ZERO, &delete_label).intrinsic_width(ctx.text, &local);
            let x1 = (footer.x0 + width).min(cancel.x0 - gap);
            buttons.push((
                hit,
                Rect::new(footer.x0, footer.y0, x1, footer.y1),
                delete_label,
                ButtonRole::Destructive,
                self.album_editor_focus == 2,
            ));
        }
        for (hit, rect, label, role, focused) in &buttons {
            let face = ButtonFace::new(*rect, label).role(*role).focused(*focused);
            let hovered = face.contains(&local, self.pointer);
            Button::paint(
                ctx.scene,
                transform,
                ctx.text,
                &local,
                &face.hovered(hovered),
            );
            self.album_editor_hits.push((*hit, *rect));
        }
    }

    /// The details editor in the inspector: the photographs' folder, people
    /// and tags, or when and where they were taken, and Save.
    #[expect(
        clippy::too_many_lines,
        reason = "the two sections share one scroll region, field rhythm and footer"
    )]
    fn draw_metadata_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        for input in [
            &mut self.metadata_folder_input,
            &mut self.metadata_people_input,
            &mut self.metadata_tags_input,
            &mut self.metadata_captured_at_input,
            &mut self.metadata_place_input,
            &mut self.metadata_latitude_input,
            &mut self.metadata_longitude_input,
        ] {
            input.apply_layout_external(Rect::ZERO);
        }
        for (_, button) in &mut self.metadata_editor_buttons {
            button.apply_layout_external(Rect::ZERO);
        }
        if !self.metadata_editor {
            self.metadata_editor_hits.clear();
            return;
        }
        let count = self.metadata_editor_ids.len();
        let title = if count == 1 {
            tr("photos-photo-details")
        } else {
            tr_count("photos-edit-count", count)
        };
        let Some((frame, local)) =
            self.paint_inspector(scene, transform, text_renderer, theme, &title)
        else {
            self.metadata_editor_hits.clear();
            return;
        };
        let rhythm = &local.tokens.spacing.rhythm;
        let (tight, related, sibling) = (
            f64::from(rhythm.tight),
            f64::from(rhythm.related),
            f64::from(rhythm.sibling),
        );
        let control_h = ui::widget::ControlSize::Regular.height(&local);
        let label_h = Self::line_of(&local, ui::TextRole::Label)
            .max(ui::widget::ControlSize::Small.height(&local));
        let body = frame.body;
        let mut hits = Vec::new();

        // The two sections.
        let strip_h = SegmentedFace::height(ui::widget::ControlSize::Regular, &local);
        let strip = Rect::new(body.x0, body.y0, body.x1, body.y0 + strip_h);
        let labels = [tr("photos-organize"), tr("photos-time-and-place")];
        let label_refs = [labels[0].as_str(), labels[1].as_str()];
        let segments = SegmentedFace::partition(
            strip,
            &label_refs,
            ui::widget::ControlSize::Regular,
            text_renderer,
            &local,
        );
        let selected =
            usize::from(self.metadata_editor_section == MetadataEditorSection::TimeAndPlace);
        let hovered = segments
            .iter()
            .position(|segment| segment.contains(self.pointer));
        ToggleGroup::paint(
            scene,
            transform,
            text_renderer,
            &local,
            &SegmentedFace::new(&segments, &label_refs, selected).hovered(hovered),
        );
        hits.extend(
            [MetadataEditorHit::Organize, MetadataEditorHit::TimeAndPlace]
                .into_iter()
                .zip(segments.iter().copied()),
        );

        let fields_top = strip.y1 + sibling;
        let region = Rect::new(body.x0, fields_top, body.x1, body.y1.max(fields_top));
        self.metadata_editor_body = region;
        let row_h = label_h + tight + control_h + sibling;
        // The map gives up height first, so in a short window the
        // coordinates under it stay in view; it never shrinks below a
        // target the pointer can still pick from.
        let preferred_map =
            (region.width() * 0.5).min(f64::from(local.tokens.metrics.panel_width_utility) * 0.6);
        let least_map = 2.0 * f64::from(local.tokens.metrics.min_pointer_target);
        let map_h = preferred_map
            .min(region.height() - 3.0 * row_h - sibling)
            .max(least_map.min(preferred_map));
        let time_and_place = self.metadata_editor_section == MetadataEditorSection::TimeAndPlace;
        let content_h = if time_and_place {
            3.0 * row_h + map_h + sibling
        } else {
            3.0 * row_h
        };
        if std::mem::take(&mut self.metadata_editor_reveal_focus) {
            let focused_row_top = match (time_and_place, self.metadata_editor_focus) {
                (false, focus @ 0..=2) | (true, focus @ 0..=1) => Some(focus as f64 * row_h),
                (true, 2 | 3) => Some(2.0 * row_h + map_h + sibling),
                _ => None,
            };
            if let Some(top) = focused_row_top {
                let bottom = top + row_h - sibling;
                if top < self.metadata_editor_scroll {
                    self.metadata_editor_scroll = top;
                } else if bottom > self.metadata_editor_scroll + region.height() {
                    self.metadata_editor_scroll = bottom - region.height();
                }
            }
        }
        self.metadata_editor_scroll = self
            .metadata_editor_scroll
            .clamp(0.0, (content_h - region.height()).max(0.0));
        let fields_y = fields_top - self.metadata_editor_scroll;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &region,
        );
        // One field: its label, and the input under it. A field a batch edit
        // can clear for every photo carries that as a toggle in its trailing
        // accessory; `clear` is whether it is on.
        let field = |scene: &mut Scene,
                     text_renderer: &mut TextRenderer,
                     input: &mut TextInput,
                     label: &str,
                     clear: Option<bool>,
                     rect: Rect| {
            PaintCtx::new(scene, transform, text_renderer, &local).text_role_in_band(
                label,
                ui::TextRole::Label,
                local.text_dim,
                rect.x0,
                Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + label_h),
                None,
            );
            if let Some(forced) = clear {
                input.set_accessory_pressed(forced);
            }
            input.apply_layout_external(Rect::new(
                rect.x0,
                rect.y0 + label_h + tight,
                rect.x1,
                rect.y0 + label_h + tight + control_h,
            ));
            input.draw(scene, transform, text_renderer, &local, presence);
        };

        let column = |index: f64| {
            let y = fields_y + index * row_h;
            Rect::new(region.x0, y, region.x1, y + row_h - sibling)
        };
        match self.metadata_editor_section {
            MetadataEditorSection::Organize => {
                let force = self.metadata_force_clear;
                for (index, (label, input)) in [
                    (tr("photos-folder"), &mut self.metadata_folder_input),
                    (tr("photos-people"), &mut self.metadata_people_input),
                    (tr("photos-tags"), &mut self.metadata_tags_input),
                ]
                .into_iter()
                .enumerate()
                {
                    let forced = force.get(index).copied().unwrap_or(false);
                    field(
                        scene,
                        text_renderer,
                        input,
                        &label,
                        Some(forced),
                        column(index as f64),
                    );
                }
            }
            MetadataEditorSection::TimeAndPlace => {
                field(
                    scene,
                    text_renderer,
                    &mut self.metadata_captured_at_input,
                    &tr("photos-captured"),
                    Some(self.metadata_force_clear[3]),
                    column(0.0),
                );
                field(
                    scene,
                    text_renderer,
                    &mut self.metadata_place_input,
                    &tr("photos-place"),
                    Some(self.metadata_force_clear[4]),
                    column(1.0),
                );
                // A pick surface for the location: the offline map, with the
                // drafted position marked.
                let map_y = fields_y + 2.0 * row_h;
                let map = Rect::new(region.x0, map_y, region.x1, map_y + map_h);
                self.metadata_location_map_bounds = map;
                self.paint_location_map(scene, transform, &local, map);
                hits.push((MetadataEditorHit::PlaceOnMap, map.intersect(region)));
                let y = map.y1 + sibling;
                let half = (region.width() - related) / 2.0;
                for (label, input, x) in [
                    (
                        tr("photos-latitude"),
                        &mut self.metadata_latitude_input,
                        region.x0,
                    ),
                    (
                        tr("photos-longitude"),
                        &mut self.metadata_longitude_input,
                        region.x0 + half + related,
                    ),
                ] {
                    field(
                        scene,
                        text_renderer,
                        input,
                        &label,
                        None,
                        Rect::new(x, y, x + half, y + row_h - sibling),
                    );
                }
            }
        }
        scene.pop_layer();
        let metrics = ScrollMetrics::new(content_h, region.height(), self.metadata_editor_scroll);
        if metrics.overflows() {
            let gutter = f64::from(local.tokens.metrics.scrollbar_gutter);
            let track = Rect::new(region.x1, region.y0, region.x1 + gutter, region.y1);
            ui::widget::scrollbar::Scrollbar::vertical()
                .draw(scene, transform, track, metrics, &local, false);
        }

        // Footer: cancel and save, trailing.
        let footer = frame.footer;
        let footer_focus = if self.metadata_editor_section == MetadataEditorSection::Organize {
            (3, 4)
        } else {
            (4, 5)
        };
        let mut x1 = footer.x1;
        for (hit, focused) in [
            (
                MetadataEditorHit::Save,
                self.metadata_editor_focus == footer_focus.1,
            ),
            (
                MetadataEditorHit::Cancel,
                self.metadata_editor_focus == footer_focus.0,
            ),
        ] {
            let label = if hit == MetadataEditorHit::Save {
                tr("photos-save-changes")
            } else {
                tr("photos-cancel")
            };
            let width = ButtonFace::new(Rect::ZERO, &label).intrinsic_width(text_renderer, &local);
            let rect = Rect::new(x1 - width, footer.y0, x1, footer.y1);
            x1 = rect.x0 - related;
            if let Some((_, button)) = self
                .metadata_editor_buttons
                .iter_mut()
                .find(|(candidate, _)| *candidate == hit)
            {
                button.set_focus(focused);
                button.apply_layout_external(rect);
                button.draw(scene, transform, text_renderer, &local, presence);
            }
        }
        self.metadata_editor_hits = hits;
    }

    /// The location pick surface: the offline land silhouettes on the
    /// raised well, and the drafted position as a ring in primary ink.
    fn paint_location_map(&self, scene: &mut Scene, transform: Affine, theme: &Theme, map: Rect) {
        use vello::peniko::Fill;
        let colors = &theme.tokens.colors;
        let water = colors.surfaces.raised;
        let land = colors.lift_on(water, 1.0);
        let radius = f64::from(theme.tokens.radii.control);
        let shape = RoundedRect::from_rect(map, radius);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        scene.fill(Fill::NonZero, transform, water, None, &map);
        let viewport = self.metadata_location_map;
        for landmass in OFFLINE_WORLD_LANDMASSES {
            let mut coast = BezPath::new();
            let mut points = 0_usize;
            for &(latitude, longitude) in *landmass {
                let location = crate::model::PhotoLocation {
                    latitude,
                    longitude,
                    label: None,
                    provenance: crate::model::LocationProvenance::default(),
                };
                let Some((x, y)) = viewport.project(&location, map.width(), map.height()) else {
                    continue;
                };
                if points == 0 {
                    coast.move_to((map.x0 + x, map.y0 + y));
                } else {
                    coast.line_to((map.x0 + x, map.y0 + y));
                }
                points += 1;
            }
            if points >= 3 {
                coast.close_path();
                scene.fill(Fill::NonZero, transform, land, None, &coast);
            }
        }
        let pin = self
            .metadata_latitude_input
            .value()
            .trim()
            .parse::<f64>()
            .ok()
            .zip(
                self.metadata_longitude_input
                    .value()
                    .trim()
                    .parse::<f64>()
                    .ok(),
            )
            .and_then(|(latitude, longitude)| {
                viewport.project(
                    &crate::model::PhotoLocation {
                        latitude,
                        longitude,
                        label: None,
                        provenance: crate::model::LocationProvenance::default(),
                    },
                    map.width(),
                    map.height(),
                )
            });
        if let Some((x, y)) = pin {
            scene.stroke(
                &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0),
                transform,
                theme.text_primary,
                None,
                &Circle::new(
                    Point::new(map.x0 + x, map.y0 + y),
                    f64::from(theme.tokens.spacing.s2),
                ),
            );
        }
        scene.pop_layer();
        scene.stroke(
            &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
            transform,
            surface::edge_on(water),
            None,
            &surface::edge_outline(map, radius),
        );
    }
}

/// How far a wheel turn scrolls, in pixels: a line notch moves three lines
/// of body text, a touchpad its own distance.
fn wheel_pixels(delta: &winit::event::MouseScrollDelta, theme: &Theme) -> f64 {
    match delta {
        winit::event::MouseScrollDelta::LineDelta(_, y) => {
            -f64::from(*y) * 3.0 * f64::from(theme.text(ui::TextRole::Body).line_height_px())
        }
        winit::event::MouseScrollDelta::PixelDelta(position) => -position.y,
    }
}

fn metadata_filter_id(folder: Option<&str>, person: Option<&str>, tag: Option<&str>) -> String {
    if let Some(folder) = folder {
        crate::derived::folder_resource_id(folder)
    } else if let Some(person) = person {
        crate::derived::person_resource_id(person)
    } else if let Some(tag) = tag {
        crate::derived::tag_resource_id(tag)
    } else {
        "all".to_owned()
    }
}

/// Where the photograph is drawn on screen: its quad clipped to the viewer.
fn displayed_image_rect(viewer: &ImageViewer, w: f64, h: f64) -> Option<Rect> {
    let rect = Widget::get_rect(viewer);
    if rect.width() <= 0.0 || rect.height() <= 0.0 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = (rect.width() / w).min(rect.height() / h) * f64::from(viewer.zoom());
    if scale <= 0.0 {
        return None;
    }
    let pan = viewer.pan();
    let (dw, dh) = (w * scale, h * scale);
    let x0 = rect.center().x - dw * 0.5 + pan.x * scale;
    let y0 = rect.center().y - dh * 0.5 + pan.y * scale;
    let shown = Rect::new(x0, y0, x0 + dw, y0 + dh).intersect(rect);
    (shown.width() > 0.0 && shown.height() > 0.0).then_some(shown)
}

/// A photograph's size in pixels as people read it: "1200 × 800".
pub(crate) fn dimensions_label(width: u32, height: u32) -> String {
    format!("{width} × {height}")
}

/// A capture time as people read it: "Jul 12, 2026 at 18:16".
fn captured_label(captured_at: &str) -> String {
    let date = timeline_date_label(captured_at);
    match captured_at.split_whitespace().nth(1) {
        Some(time) if !time.is_empty() => tr_args(
            "photos-date-at-time",
            &[
                ("date", ArgValue::Text(date)),
                ("time", ArgValue::Text(time.to_owned())),
            ],
        ),
        _ => date,
    }
}

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
/// Native controls layered over the custom metadata inspector. The inspector
/// owns responsive geometry; `Button` owns interaction states, focus, action
/// publication, tooltips, and the accessible name. This keeps the sheet from
/// becoming a painted collection of fake controls merely because it is docked
/// over the photo canvas.
fn build_metadata_editor_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(MetadataEditorHit, Button)> {
    [
        (MetadataEditorHit::Organize, "organize", "photos-organize"),
        (
            MetadataEditorHit::TimeAndPlace,
            "time-place",
            "photos-time-and-place",
        ),
        (
            MetadataEditorHit::PlaceOnMap,
            "place-on-map",
            "photos-place-on-map",
        ),
        (MetadataEditorHit::Cancel, "cancel", "photos-cancel"),
        (MetadataEditorHit::Save, "save", "photos-save-changes"),
    ]
    .into_iter()
    .map(|(hit, id, label)| {
        let button = Button::new(&format!("photos-metadata-{id}"), label)
            .tooltip(label)
            .with_accessible_name(label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

/// A metadata field whose trailing accessory toggles "clear this for every
/// photo in the batch": a field's own control, not a separate button.
fn clear_toggled_input(
    id: &str,
    clear: MetadataEditorHit,
    pending: &Rc<Cell<Option<MetadataEditorHit>>>,
) -> TextInput {
    let name = tr(match clear {
        MetadataEditorHit::ClearFolder => "photos-clear-folder",
        MetadataEditorHit::ClearPeople => "photos-clear-people",
        MetadataEditorHit::ClearTags => "photos-clear-tags",
        MetadataEditorHit::ClearCapturedAt => "photos-clear-captured",
        _ => "photos-clear-location",
    });
    let slot = Rc::clone(pending);
    TextInput::local().with_id(id).with_trailing_accessory(
        Icon::X,
        &name,
        ui::widget::AccessoryAction::Toggle {
            pressed: false,
            on_toggle: Box::new(move |_| slot.set(Some(clear))),
        },
    )
}

fn build_collection_page_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(CollectionPageHit, Button)> {
    [
        (
            CollectionPageHit::PeoplePrevious,
            "photos-people-previous",
            "photos-previous",
        ),
        (
            CollectionPageHit::PeopleNext,
            "photos-people-next",
            "photos-next",
        ),
        (
            CollectionPageHit::PlacesPrevious,
            "photos-places-previous",
            "photos-previous",
        ),
        (
            CollectionPageHit::PlacesNext,
            "photos-places-next",
            "photos-next",
        ),
    ]
    .into_iter()
    .map(|(hit, id, label)| {
        let button = Button::new(id, label)
            .tooltip(label)
            .with_accessible_name(label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

/// What a folder import card's button says.
fn directory_status_label(hit: DirectoryStatusHit) -> String {
    tr(match hit {
        DirectoryStatusHit::Stop => "photos-import-stop",
        DirectoryStatusHit::Details => "photos-import-details",
        DirectoryStatusHit::Dismiss => "photos-import-dismiss",
    })
}

fn build_directory_import_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(DirectoryStatusHit, Button)> {
    [
        (DirectoryStatusHit::Stop, "photos-directory-stop"),
        (DirectoryStatusHit::Details, "photos-directory-details"),
        (DirectoryStatusHit::Dismiss, "photos-directory-dismiss"),
    ]
    .into_iter()
    .map(|(hit, id)| {
        let label = directory_status_label(hit);
        let button = Button::new(id, &label)
            .tooltip(&label)
            .with_accessible_name(&label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

/// Shared retained controls placed over the custom timeline geometry.  The
/// timeline owns placement, while Button owns its visual states, names and
/// minimum target treatment.
fn build_library_analysis_buttons(
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Vec<(LibraryAnalysisHit, Button)> {
    [
        (
            LibraryAnalysisHit::Start,
            "photos-analysis-start",
            Icon::Sparkles,
        ),
        (
            LibraryAnalysisHit::Pause,
            "photos-analysis-pause",
            Icon::Pause,
        ),
        (
            LibraryAnalysisHit::Resume,
            "photos-analysis-resume",
            Icon::Play,
        ),
        (
            LibraryAnalysisHit::RetryFailures,
            "photos-analysis-retry",
            Icon::Refresh,
        ),
    ]
    .into_iter()
    .map(|(hit, id, icon)| {
        let label = tr(match hit {
            LibraryAnalysisHit::Start => "photos-analysis-start",
            LibraryAnalysisHit::Pause => "photos-analysis-pause",
            LibraryAnalysisHit::Resume => "photos-analysis-resume",
            LibraryAnalysisHit::RetryFailures => "photos-analysis-retry",
        });
        let button = Button::new(id, &label)
            .with_icon(icon)
            .tooltip(&label)
            .with_accessible_name(&label);
        (hit, bound(button, &hit.effect(), actions))
    })
    .collect()
}

impl Hit {
    fn effect(self) -> Option<PhotosEffect> {
        Some(match self {
            Self::Open => PhotosEffect::Import { path: None },
            Self::Library => PhotosEffect::ViewLibrary,
            Self::RotateLeft => PhotosEffect::RotateShownLeft,
            Self::RotateRight => PhotosEffect::RotateShownRight,
            Self::CropToView => PhotosEffect::CropShownToView,
            Self::Reset => PhotosEffect::ResetShownEdits,
            Self::Copy => PhotosEffect::Copy,
            Self::Share => PhotosEffect::ShareShown,
            Self::Save => PhotosEffect::Save { path: None },
            Self::PreviousPage => PhotosEffect::PreviousLibraryPage,
            Self::NextPage => PhotosEffect::NextLibraryPage,
            Self::NewAlbum => PhotosEffect::NewAlbum,
            Self::EditAlbum => PhotosEffect::EditAlbum,
            Self::Smaller => PhotosEffect::ShowFewerPhotos,
            Self::Larger => PhotosEffect::ShowMorePhotos,
            Self::PeopleHidden => PhotosEffect::ToggleHiddenSuggestions,
            _ => return None,
        })
    }
}

/// The effect each retained control invokes, and back: a button is bound to
/// its effect through the framework, and the effect's handler posts the
/// control's intent for the next sync, which has the document.
impl MetadataEditorHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::Save => PhotosEffect::SaveDetails,
            Self::Cancel => PhotosEffect::CancelDetails,
            Self::Organize => PhotosEffect::ShowDetailsOrganize,
            Self::TimeAndPlace => PhotosEffect::ShowDetailsTimeAndPlace,
            Self::ClearFolder => PhotosEffect::ClearDraftFolder,
            Self::ClearPeople => PhotosEffect::ClearDraftPeople,
            Self::ClearTags => PhotosEffect::ClearDraftTags,
            Self::ClearCapturedAt => PhotosEffect::ClearDraftCapturedAt,
            Self::ClearLocation => PhotosEffect::ClearDraftLocation,
            Self::PlaceOnMap => PhotosEffect::PlaceDraftOnMap,
        }
    }
}

impl CollectionPageHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::AlbumsPrevious => PhotosEffect::PreviousAlbumsPage,
            Self::AlbumsNext => PhotosEffect::NextAlbumsPage,
            Self::PeoplePrevious => PhotosEffect::PreviousPeoplePage,
            Self::PeopleNext => PhotosEffect::NextPeoplePage,
            Self::PlacesPrevious => PhotosEffect::PreviousPlacesPage,
            Self::PlacesNext => PhotosEffect::NextPlacesPage,
        }
    }
}

impl DirectoryStatusHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::Stop => PhotosEffect::StopImport,
            Self::Details => PhotosEffect::ShowImportDetails,
            Self::Dismiss => PhotosEffect::DismissImport,
        }
    }
}

impl LibraryAnalysisHit {
    fn effect(self) -> PhotosEffect {
        match self {
            Self::Start => PhotosEffect::AnalyzeLibrary,
            Self::Pause => PhotosEffect::PauseLibraryAnalysis,
            Self::Resume => PhotosEffect::ResumeLibraryAnalysis,
            Self::RetryFailures => PhotosEffect::RetryLibraryAnalysisFailures,
        }
    }
}

impl PeopleIntent {
    fn effect(&self) -> PhotosEffect {
        match self.clone() {
            Self::Back => PhotosEffect::PeopleBack,
            Self::ToggleRecognition => PhotosEffect::TogglePeopleRecognition,
            Self::Scan => PhotosEffect::AnalyzePeopleLibrary,
            Self::CancelScan => PhotosEffect::CancelPeopleScan,
            Self::OpenApproved(person) => PhotosEffect::OpenApprovedPerson { person },
            Self::OpenSuggestion(cluster) => PhotosEffect::OpenSuggestion { cluster },
            Self::BrowseApproved => PhotosEffect::BrowseApprovedPeople,
            Self::SaveName => PhotosEffect::SavePersonName,
            Self::BeginMerge => PhotosEffect::BeginPeopleMerge,
            Self::MergeApprovedInto(person) => PhotosEffect::MergeApprovedInto { person },
            Self::ApproveSuggestion => PhotosEffect::ApproveSuggestion,
            Self::RejectSuggestion => PhotosEffect::RejectSuggestion,
            Self::HideSuggestion => PhotosEffect::HideSuggestion,
            Self::RestoreSuggestion => PhotosEffect::RestoreSuggestion,
            Self::MergeSuggestionWith(person) => PhotosEffect::MergeSuggestionWith { person },
            Self::ToggleHidden => PhotosEffect::ToggleHiddenSuggestions,
            Self::Page(forward) => PhotosEffect::PeoplePage { forward },
            Self::DeleteLocalData => PhotosEffect::DeleteLocalPeopleData,
            Self::ConfirmDeleteLocalData => PhotosEffect::ConfirmDeleteLocalPeopleData,
        }
    }
}

/// The intent of the retained control `effect` stands for, when it is one.
enum Control {
    Toolbar(Hit),
    Album(AlbumEditorHit),
    Details(MetadataEditorHit),
    Collection(CollectionPageHit),
    Directory(DirectoryStatusHit),
    People(PeopleIntent),
}

impl Control {
    fn of(effect: &PhotosEffect) -> Option<Self> {
        use PhotosEffect as E;
        Some(match effect.clone() {
            E::RotateShownLeft => Self::Toolbar(Hit::RotateLeft),
            E::RotateShownRight => Self::Toolbar(Hit::RotateRight),
            E::CropShownToView => Self::Toolbar(Hit::CropToView),
            E::ResetShownEdits => Self::Toolbar(Hit::Reset),
            E::ShareShown => Self::Toolbar(Hit::Share),
            E::ToggleMoreMenu => Self::Toolbar(Hit::More),
            E::NewAlbum => Self::Album(AlbumEditorHit::New),
            E::EditAlbum => Self::Album(AlbumEditorHit::Edit),
            E::PreviousLibraryPage => Self::Album(AlbumEditorHit::PreviousPage),
            E::NextLibraryPage => Self::Album(AlbumEditorHit::NextPage),
            E::SaveAlbumDraft => Self::Album(AlbumEditorHit::Save),
            E::CancelAlbumDraft => Self::Album(AlbumEditorHit::Cancel),
            E::DeleteAlbumDraft => Self::Album(AlbumEditorHit::Delete),
            E::ConfirmAlbumDelete => Self::Album(AlbumEditorHit::ConfirmDelete),
            E::SaveDetails => Self::Details(MetadataEditorHit::Save),
            E::CancelDetails => Self::Details(MetadataEditorHit::Cancel),
            E::ShowDetailsOrganize => Self::Details(MetadataEditorHit::Organize),
            E::ShowDetailsTimeAndPlace => Self::Details(MetadataEditorHit::TimeAndPlace),
            E::ClearDraftFolder => Self::Details(MetadataEditorHit::ClearFolder),
            E::ClearDraftPeople => Self::Details(MetadataEditorHit::ClearPeople),
            E::ClearDraftTags => Self::Details(MetadataEditorHit::ClearTags),
            E::ClearDraftCapturedAt => Self::Details(MetadataEditorHit::ClearCapturedAt),
            E::ClearDraftLocation => Self::Details(MetadataEditorHit::ClearLocation),
            E::PlaceDraftOnMap => Self::Details(MetadataEditorHit::PlaceOnMap),
            E::ShowFewerPhotos => Self::Toolbar(Hit::Smaller),
            E::ShowMorePhotos => Self::Toolbar(Hit::Larger),
            E::PreviousAlbumsPage => Self::Collection(CollectionPageHit::AlbumsPrevious),
            E::NextAlbumsPage => Self::Collection(CollectionPageHit::AlbumsNext),
            E::PreviousPeoplePage => Self::Collection(CollectionPageHit::PeoplePrevious),
            E::NextPeoplePage => Self::Collection(CollectionPageHit::PeopleNext),
            E::PreviousPlacesPage => Self::Collection(CollectionPageHit::PlacesPrevious),
            E::NextPlacesPage => Self::Collection(CollectionPageHit::PlacesNext),
            E::StopImport => Self::Directory(DirectoryStatusHit::Stop),
            E::ShowImportDetails => Self::Directory(DirectoryStatusHit::Details),
            E::DismissImport => Self::Directory(DirectoryStatusHit::Dismiss),
            E::PeopleBack => Self::People(PeopleIntent::Back),
            E::TogglePeopleRecognition => Self::People(PeopleIntent::ToggleRecognition),
            E::CancelPeopleScan => Self::People(PeopleIntent::CancelScan),
            E::OpenApprovedPerson { person } => Self::People(PeopleIntent::OpenApproved(person)),
            E::OpenSuggestion { cluster } => Self::People(PeopleIntent::OpenSuggestion(cluster)),
            E::BrowseApprovedPeople => Self::People(PeopleIntent::BrowseApproved),
            E::SavePersonName => Self::People(PeopleIntent::SaveName),
            E::BeginPeopleMerge => Self::People(PeopleIntent::BeginMerge),
            E::MergeApprovedInto { person } => {
                Self::People(PeopleIntent::MergeApprovedInto(person))
            }
            E::ApproveSuggestion => Self::People(PeopleIntent::ApproveSuggestion),
            E::RejectSuggestion => Self::People(PeopleIntent::RejectSuggestion),
            E::HideSuggestion => Self::People(PeopleIntent::HideSuggestion),
            E::RestoreSuggestion => Self::People(PeopleIntent::RestoreSuggestion),
            E::MergeSuggestionWith { person } => {
                Self::People(PeopleIntent::MergeSuggestionWith(person))
            }
            E::ToggleHiddenSuggestions => Self::People(PeopleIntent::ToggleHidden),
            E::PeoplePage { forward } => Self::People(PeopleIntent::Page(forward)),
            E::DeleteLocalPeopleData => Self::People(PeopleIntent::DeleteLocalData),
            E::ConfirmDeleteLocalPeopleData => Self::People(PeopleIntent::ConfirmDeleteLocalData),
            _ => return None,
        })
    }
}

/// `button`, bound to `effect` through the framework's door once the surface
/// is attached; until then it is drawn and reached by its semantics alone.
fn bound(
    button: Button,
    effect: &PhotosEffect,
    actions: Option<&ui::framework::Actions<crate::model::PhotoLibrary>>,
) -> Button {
    match actions {
        Some(actions) => actions.bind_effect_invocation(button, effect),
        None => button,
    }
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
    format!("{}:480x480", decode_key(open))
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
    let known_month = month.len() == 2
        && month.bytes().all(|byte| byte.is_ascii_digit())
        && ("01"..="12").contains(&month);
    if known_month
        && day_of_month.len() == 2
        && day_of_month.bytes().all(|byte| byte.is_ascii_digit())
    {
        tr_args(
            "photos-day-label",
            &[
                (
                    "month",
                    ArgValue::Text(tr(&format!("photos-month-{month}"))),
                ),
                ("day", ArgValue::Text(day_of_month.to_owned())),
                ("year", ArgValue::Text(year.to_owned())),
            ],
        )
    } else {
        day
    }
}

fn collection_page_access_node(hit: CollectionPageHit, bounds: Rect, enabled: bool) -> AccessNode {
    let (name, target) = match hit {
        CollectionPageHit::AlbumsPrevious => ("photos-a11y-previous-albums", "albums-previous"),
        CollectionPageHit::AlbumsNext => ("photos-a11y-more-albums", "albums-next"),
        CollectionPageHit::PeoplePrevious => ("photos-a11y-previous-people", "people-previous"),
        CollectionPageHit::PeopleNext => ("photos-a11y-more-people", "people-next"),
        CollectionPageHit::PlacesPrevious => ("photos-a11y-previous-places", "places-previous"),
        CollectionPageHit::PlacesNext => ("photos-a11y-more-places", "places-next"),
    };
    AccessNode::new(Role::Button)
        .with_name(tr(name))
        .with_description(tr("photos-a11y-page-hint"))
        .with_bounds(bounds)
        .with_state(AccessState {
            focusable: enabled,
            disabled: !enabled,
            ..Default::default()
        })
        .with_action_target(format!("photos:collection-page:{target}"))
}

/// Preserve a capture day as the timeline's chronological identity. Display
/// formatting must never become the sort/group key: doing so would collapse
/// every "Jun" from different years into one ambiguous bucket.
fn capture_day(captured_at: &str) -> String {
    // The library stores capture times as ISO (`2026-07-12T18:16`); its day
    // is the date before the time.
    if let Some(day) = crate::derived::timeline_day_from_capture(captured_at) {
        return day;
    }
    captured_at
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("Undated")
        .to_owned()
}

/// Read at most the encoded-image budget plus one sentinel byte. A hostile
/// file can otherwise exhaust memory before the decoder gets a chance to
/// enforce its decoded-pixel limit.
impl PhotosApp {
    /// Import the photographs a contents chooser delivered.
    fn drain_contents_picker(&mut self) {
        let Some(rx) = self.contents_picker.as_ref() else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::ContentsOutcome::Failed(tr("photos-picker-failed"))
            }
        };
        self.contents_picker = None;
        let stale = std::mem::take(&mut self.picker_stale);
        self.import_content = None;
        let invocation = self.resource_invocation.take();
        let result = match outcome {
            ui::portal::ContentsOutcome::Picked(files) if stale => {
                let message = tr_count("photos-import-stale-count", files.len());
                self.notice = Some(message.clone());
                Err(("photos.stale_selection", message))
            }
            ui::portal::ContentsOutcome::Picked(files) => {
                for file in files {
                    self.open_contents(&file.name, &file.bytes);
                }
                if self.notice.is_none() {
                    Ok(())
                } else {
                    Err(("photos.open_failed", tr("photos-open-failed")))
                }
            }
            ui::portal::ContentsOutcome::Cancelled => {
                if let Some(invocation) = invocation {
                    self.settle(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some(tr("photos-open-cancelled")),
                    });
                }
                return;
            }
            ui::portal::ContentsOutcome::Failed(message) => {
                self.notice = Some(format!("{}: {message}", tr("photos-open-failed")));
                Err(("photos.portal_failed", message))
            }
        };
        if let Some(invocation) = invocation {
            self.settle(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err((code, message)) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: code.into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
    }
}

fn bounded_file_bytes(path: &str) -> Option<Vec<u8>> {
    crate::files::read_native(std::path::Path::new(path), pixels::MAX_ENCODED_BYTES as u64)
}

impl PhotosApp {
    fn reconcile_content(&mut self) {
        let current = LiveContent {
            library: (*self.presented()).clone(),
        };
        if self
            .import_content
            .as_ref()
            .is_some_and(|old| old != &current)
        {
            self.picker_stale = true;
            if let Some((invocation, _)) = self.pending_resource_open.take() {
                self.settle(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some(tr("photos-import-stale")),
                });
                self.import_content = None;
            }
        }
        if let Some(old) = &self.observed_content
            && old != &current
        {
            self.pending.take();
            self.library_cards.clear();
            self.library_details_cards.clear();
            // Edits to the pixels leave the details a draft describes alone.
            let filed = |library: &crate::model::PhotoLibrary, id: &str| {
                library.photo(id).cloned().map(|mut photo| {
                    photo.edits = Edits::default();
                    photo
                })
            };
            let metadata_changed = self
                .metadata_editor_ids
                .iter()
                .any(|id| filed(&old.library, id) != filed(&current.library, id));
            let album_changed = self
                .album_editor_id
                .as_ref()
                .is_some_and(|id| old.library.album(id) != current.library.album(id))
                || self
                    .album_editor_members
                    .iter()
                    .any(|id| current.library.photo(id).is_none());
            // A photograph removed from the library closes in this window.
            let shown_removed = self.open_photo.as_ref().is_some_and(|id| {
                old.library.photo(id).is_some() && current.library.photo(id).is_none()
            });
            if shown_removed {
                self.open_photo = None;
            }
            if metadata_changed {
                self.cancel_metadata_editor();
                self.notice = Some(tr("photos-details-conflict"));
            }
            if album_changed {
                self.cancel_album_editor();
                self.notice = Some(tr("photos-album-changed-elsewhere"));
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
        self.refresh();
    }

    /// Take completed thumbnail work without blocking the UI thread.
    fn drain_thumbnail_results(&mut self) {
        let Some(generation) = self.thumbnail_generation else {
            return;
        };
        let settled: Vec<(String, Option<thumbnail::ThumbState>)> = self
            .thumbnail_keys
            .iter()
            .filter_map(|(id, key)| match self.thumbnail_service.peek(key) {
                Some(state) if state.is_loading() => None,
                state => Some((id.clone(), state)),
            })
            .collect();
        for (id, state) in settled {
            self.thumbnail_keys.remove(&id);
            match state {
                // Dropped from the shared queue: forget it here too, so the
                // next schedule asks again.
                None => {
                    self.thumbnail_queued.remove(&id);
                }
                Some(state) => self.accept_thumbnail_result(ThumbnailResult {
                    generation,
                    id,
                    preview: state.ready().and_then(|preview| {
                        pixels::Rgba::new(preview.width, preview.height, preview.rgba.clone())
                    }),
                }),
            }
        }
    }

    /// Accept a worker result only when it still belongs to the materialised
    /// library slice. This is the generation fence that makes scrolling,
    /// filtering and collaboration safe while a decode is in flight.
    /// The decoded preview of the photograph `photo_id`, once it arrived.
    fn thumbnail_for(&self, photo_id: &str) -> Option<&ImageData> {
        self.thumbnail_resource_keys
            .get(photo_id)
            .and_then(|key| self.library_thumbnails.get(key))
    }

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

    fn retained_buttons_mut(&mut self) -> Vec<&mut Button> {
        let mut buttons = vec![&mut self.chrome.back, &mut self.chrome.search];
        buttons.extend(self.chrome.history.button_mut());
        buttons.extend(
            self.directory_import_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.library_analysis_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.collection_page_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.metadata_editor_buttons
                .iter_mut()
                .map(|(_, button)| button),
        );
        buttons.extend(
            self.people_controls
                .iter_mut()
                .map(|control| &mut control.button),
        );
        buttons
    }

    /// Apply callbacks after the host has admitted and dispatched an emitted
    /// control action. Pointer handling only queues the action; the callback
    /// runs after that event has returned, so `sync` is the common completion
    /// boundary for pointer, keyboard, accessibility, and agent activation.
    fn drain_action_control_pending(&mut self) {
        self.drain_pending();
        if let Some(hit) = self.directory_import_pending.take() {
            self.activate_directory_status(hit);
        }
        if let Some(hit) = self.library_analysis_pending.take() {
            self.activate_library_analysis(hit);
        }
        if let Some(hit) = self.collection_page_pending.take() {
            self.page_collection(hit);
        }
        if let Some(hit) = self.metadata_editor_pending.take() {
            self.activate_metadata_editor_hit(hit);
        }
        if let Some(hit) = self.album_editor_pending.take() {
            match hit {
                AlbumEditorHit::New => self.begin_new_album(),
                AlbumEditorHit::Edit => self.begin_edit_album(),
                AlbumEditorHit::Save => self.save_album_editor(),
                AlbumEditorHit::Cancel => self.cancel_album_editor(),
                AlbumEditorHit::Delete => {
                    self.album_delete_confirm = true;
                    self.notice = Some(
                        "Press Delete album? again to keep photographs and delete only the album"
                            .to_owned(),
                    );
                }
                AlbumEditorHit::ConfirmDelete => self.delete_active_album(),
                AlbumEditorHit::PreviousPage => self.page_library(false),
                AlbumEditorHit::NextPage => self.page_library(true),
            }
        }
        let people = self.people_pending.borrow_mut().take();
        if let Some(intent) = people {
            self.handle_people_intent(intent);
        }
    }

    /// Fill the small bounded worker queue each synchronization. This keeps
    /// decoding off the UI thread while avoiding a 24-frame wait for a single
    /// visible shelf to become useful after a scroll or first library open.
    fn schedule_library_thumbnail(&mut self, sources: Vec<LibraryPhoto>, document_changed: bool) {
        self.thumbnail_resource_keys = sources
            .iter()
            .map(|photo| (photo.id.clone(), thumbnail_key(&photo.image)))
            .collect();
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
                self.withdraw_thumbnail_requests(&[]);
                self.thumbnail_generation = None;
                self.thumbnail_visible_ids = ids;
                self.thumbnail_queued.clear();
                self.library_thumbnails.clear();
                self.thumbnail_unavailable.clear();
                return;
            };
            self.withdraw_thumbnail_requests(&ids);
            self.thumbnail_generation = Some(next_generation);
            self.thumbnail_visible_ids = ids;
            self.thumbnail_queued.clear();
            if document_changed {
                self.library_thumbnails.clear();
                // Bytes behind an unchanged address may have arrived, and a
                // legacy path may name new pixels: ask the pipeline afresh.
                for photo in &sources {
                    self.thumbnail_service
                        .forget(&thumbnail_request(&photo.image, &self.content).key());
                }
            }
            self.library_thumbnails
                .retain(|id, _| self.thumbnail_visible_ids.contains(id));
            self.thumbnail_unavailable
                .retain(|id| self.thumbnail_visible_ids.contains(id));
        }
        if self.thumbnail_generation.is_none() {
            return;
        }
        for photo in sources {
            // The shared queue is bounded, but its worker can take a job
            // between two requests. Without this independent accounting fence
            // a fast worker lets one sync enqueue an arbitrarily large visible
            // slice while `thumbnail_queued` continues to grow. Keep the whole
            // in-flight generation strictly bounded.
            if self.thumbnail_queued.len() >= THUMBNAIL_QUEUE_LIMIT {
                break;
            }
            if self
                .library_thumbnails
                .contains_key(&thumbnail_key(&photo.image))
                || self
                    .thumbnail_unavailable
                    .contains(&thumbnail_key(&photo.image))
                || self.thumbnail_queued.contains(&thumbnail_key(&photo.image))
            {
                continue;
            }
            let id = thumbnail_key(&photo.image);
            let request = thumbnail_request(&photo.image, &self.content);
            self.thumbnail_keys.insert(id.clone(), request.key());
            self.thumbnail_service.request(request);
            self.thumbnail_queued.insert(id);
        }
    }

    /// Withdraw queued previews that are no longer on screen. Work already
    /// running settles into the shared cache; its result is simply not taken.
    fn withdraw_thumbnail_requests(&mut self, still_visible: &[String]) {
        let service = &self.thumbnail_service;
        self.thumbnail_keys.retain(|id, key| {
            let keep = still_visible.contains(id);
            if !keep {
                service.cancel(key);
            }
            keep
        });
    }
}

impl Widget for PhotosApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    /// Shared controls remain retained children even in the painted media
    /// surface, with their own focus, action custody and accessibility.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.chrome.search, &self.chrome.toolbar];
        if self.header_back_visible() {
            children.push(&self.chrome.back);
        }
        children.extend(
            self.chrome
                .history
                .button()
                .map(|button| button as &dyn Widget),
        );
        children.extend(self.empty_state.as_ref().map(|empty| empty as &dyn Widget));
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let back_visible = self.header_back_visible();
        let mut children: Vec<&mut (dyn Widget + '_)> =
            vec![&mut self.chrome.search, &mut self.chrome.toolbar];
        if back_visible {
            children.push(&mut self.chrome.back);
        }
        children.extend(
            self.chrome
                .history
                .button_mut()
                .map(|button| button as &mut (dyn Widget + '_)),
        );
        children.extend(
            self.empty_state
                .as_mut()
                .map(|empty| empty as &mut (dyn Widget + '_)),
        );
        children
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.places_motion_reduced = reduced;
        if reduced {
            // Reduced motion keeps the camera operation immediate and every
            // marker fully present; only the decorative settle cue disappears.
            self.places_camera_feedback = None;
        }
    }

    /// A preview of a photograph on screen is still being decoded: a capture
    /// waits for it rather than showing a placeholder.
    fn has_pending_visual_work(&self) -> bool {
        self.staged.is_some()
            || !self.thumbnail_queued.is_empty()
            || (self.thumbnail_generation.is_some()
                && self.thumbnail_visible_ids.iter().any(|id| {
                    !self.library_thumbnails.contains_key(id)
                        && !self.thumbnail_unavailable.contains(id)
                }))
    }

    fn needs_animation_frame(&self) -> bool {
        let now = ui::time::now();
        self.chrome.drawer.is_animating()
            || self.chrome.inspector.is_animating()
            || self.status_bubble.needs_frame(now)
            || (!self.places_motion_reduced
                && (self
                    .places_camera_feedback
                    .is_some_and(|feedback| !feedback.done(now))
                    || self.people_detail_transition.is_animating(now)))
    }

    fn take_structure_dirty(&mut self) -> bool {
        let viewer = self
            .viewer
            .as_mut()
            .is_some_and(Widget::take_structure_dirty);
        std::mem::take(&mut self.structure_dirty) | viewer
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn layout_needs_measure(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "synchronization derives the bounded library, filters, search, and thumbnail work from one document snapshot"
    )]
    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        Widget::sync(&mut self.chrome.toolbar, doc, path);
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.follow_cancellations();
        self.open_launched();
        self.reconcile_content();
        self.apply_staged();
        self.drain_action_control_pending();
        self.drain_opens();
        self.drain_analysis_results();
        if let Some((invocation, path)) = self.pending_resource_open.take() {
            self.import_content = None;
            self.open_path(&path);
            self.settle(if self.notice.is_none() {
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
        self.drain_picker();
        self.drain_directory_import();
        self.refresh();
        let revision = Some(self.presented_revision);
        let document_changed = revision != self.search_revision;
        if document_changed {
            self.search_refresh_error = match self.refresh_library_index() {
                Ok(()) => None,
                Err(error) => Some(tr_with("photos-search-stale", "reason", &error.to_string())),
            };
            self.thumbnail_unavailable.clear();
            self.search_revision = revision;
        }
        let library = self.presented();
        self.library_analysis_coverage = library.photos.values().fold(
            (0usize, 0usize, 0usize),
            |(analyzed, available, awaiting), photo| {
                if photo.analysis().is_some_and(|analysis| {
                    analysis
                        .vision_provenance_for_content(photo.image.content.as_deref())
                        .is_some()
                }) {
                    (analyzed + 1, available, awaiting)
                } else if crate::model::bytes_of(&photo.image, &self.content).is_some() {
                    (analyzed, available + 1, awaiting)
                } else {
                    (analyzed, available, awaiting + 1)
                }
            },
        );
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
            self.notice = Some(tr("photos-album-gone"));
        }
        let membership = self.active_album.as_ref().and_then(|id| {
            library
                .albums
                .values()
                .find(|album| &album.id == id)
                .map(|album| &album.photo_ids)
        });
        let matches_filter = |photo: &LibraryPhoto| {
            membership.is_none_or(|ids| ids.contains(&photo.id))
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
                        .vision
                        .as_ref()
                        .is_some_and(|vision| vision.objects.contains(object))
                })
                && self.active_pet.as_ref().is_none_or(|pet| {
                    photo
                        .vision
                        .as_ref()
                        .is_some_and(|vision| vision.pets.contains(pet))
                })
        };
        self.library_total = library
            .photos
            .values()
            .filter(|photo| matches_filter(photo))
            .count();
        self.library_favorite_count = library
            .photos
            .values()
            .filter(|photo| photo.favorite)
            .count();
        self.library_offset = self
            .library_offset
            .min(self.library_total.saturating_sub(1));
        let mut timeline_items: Vec<_> = library
            .photos
            .values()
            .filter(|photo| matches_filter(photo))
            .map(|photo| LibraryItem {
                id: photo.id.clone(),
                name: photo.image.name.clone(),
                favorite: photo.favorite,
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
        let projection_filter = format!(
            "{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{:?}\u{1f}{}",
            self.active_album,
            self.active_folder,
            self.active_person,
            self.active_tag,
            self.active_object,
            self.active_pet,
            self.active_favorites
        );
        if self.timeline_projection_revision != revision
            || self.timeline_projection_filter != projection_filter
        {
            self.timeline_projection.reconcile(
                library
                    .photos
                    .values()
                    .filter(|photo| matches_filter(photo))
                    .map(|photo| TimelinePhoto {
                        id: photo.id.clone(),
                        name: photo.image.name.clone(),
                        favorite: photo.favorite,
                        day: photo
                            .captured_at
                            .as_deref()
                            .map_or_else(|| "Undated".to_owned(), capture_day),
                        captured_at: photo.captured_at.clone().unwrap_or_default(),
                        aspect_ratio: if photo.image.width > 0 && photo.image.height > 0 {
                            f64::from(photo.image.width) / f64::from(photo.image.height)
                        } else {
                            1.5
                        },
                    }),
            );
            self.timeline_projection_revision = revision;
            self.timeline_projection_filter = projection_filter;
            self.library_total = self.timeline_projection.len();
        }
        // Markers are rebuilt from exact pixel day offsets during draw, once
        // the real viewport and responsive geometry are known.
        self.timeline_index_markers.clear();
        let visible_ids: HashSet<_> = self
            .timeline_projection
            .visible_photos(self.timeline_scroll_y, self.timeline_viewport_height)
            .into_iter()
            .map(|entry| entry.photo.id.clone())
            .collect();
        self.library_items = if visible_ids.is_empty() {
            timeline_items.into_iter().take(120).collect()
        } else {
            timeline_items
                .into_iter()
                .filter(|item| visible_ids.contains(&item.id))
                .take(120)
                .collect()
        };
        self.places_items = library
            .photos
            .values()
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
            .values()
            .map(|album| (album.id.clone(), album.name.clone(), album.photo_ids.len()))
            .collect();
        self.library_album_covers = library
            .albums
            .values()
            .map(|album| {
                let covers = album
                    .photo_ids
                    .iter()
                    .filter(|id| library.photo(id).is_some())
                    .take(4)
                    .cloned()
                    .collect();
                (album.id.clone(), covers)
            })
            .collect();
        let mut folders = std::collections::BTreeMap::<String, usize>::new();
        let mut people = std::collections::BTreeMap::<String, usize>::new();
        let mut tags = std::collections::BTreeMap::<String, usize>::new();
        for photo in library.all_photos() {
            if let Some(folder) = &photo.folder {
                *folders.entry(folder.clone()).or_default() += 1;
            }
            for person in photo.people.iter() {
                *people.entry(person.clone()).or_default() += 1;
            }
            for tag in photo.tags.iter() {
                *tags.entry(tag.clone()).or_default() += 1;
            }
        }
        self.people_approved = crate::people::approved_people(&library);
        self.library_folders = folders.into_iter().take(6).collect();
        self.library_people = people.into_iter().collect();
        self.albums_page = self
            .albums_page
            .min(self.library_albums.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.people_page = self
            .people_page
            .min(self.library_people.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.places_page = self
            .places_page
            .min(self.places_items.len().saturating_sub(1) / COLLECTION_PAGE_SIZE);
        self.library_person_covers.clear();
        for (person, _) in self
            .library_people
            .iter()
            .skip(self.people_page * COLLECTION_PAGE_SIZE)
            .take(COLLECTION_PAGE_SIZE)
        {
            if let Some(photo) = library
                .photos
                .values()
                .find(|photo| photo.people.iter().any(|label| label == person))
            {
                self.library_person_covers
                    .insert(person.clone(), photo.id.clone());
            }
        }
        // The dedicated People management surface may show approved people
        // outside the current browse-shelf page, so add their bounded covers
        // without changing collection-page navigation.
        for person in &self.people_approved {
            if self.library_person_covers.contains_key(&person.label) {
                continue;
            }
            if let Some(photo) = library
                .photos
                .values()
                .find(|photo| photo.people.iter().any(|label| label == &person.label))
            {
                self.library_person_covers
                    .insert(person.label.clone(), photo.id.clone());
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
        } else if self.library_destination == LibraryDestination::Albums {
            thumbnail_sources = self
                .library_albums
                .iter()
                .filter_map(|(id, _, _)| self.library_album_covers.get(id)?.first())
                .filter_map(|photo_id| library.photo(photo_id).cloned())
                .take(24)
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
        if self.library_destination == LibraryDestination::People {
            for cluster in self.people_controller.all_clusters() {
                for photo_id in cluster.visible_photo_ids().into_iter().take(4) {
                    if thumbnail_sources.iter().all(|photo| photo.id != photo_id)
                        && let Some(photo) = library.photo(&photo_id).cloned()
                    {
                        thumbnail_sources.push(photo);
                    }
                }
            }
            thumbnail_sources.truncate(40);
        }
        self.drain_thumbnail_results();
        self.schedule_library_thumbnail(thumbnail_sources, document_changed);
        self.refresh_chrome_text(doc);
        self.refresh_toolbar();
        self.refresh_empty_state(doc);
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
        self.layout_theme = theme.clone();
        self.chrome.history.tree_rebuilt();
        self.chrome.drawer.scale_for(theme);
        // A docked sidebar or inspector takes its column beside the content;
        // in a narrow window each floats over it and takes nothing.
        self.chrome.inspector.scale_for(theme);
        self.sync_inspector();
        let content = self.chrome.drawer.resolve(self.rect);
        let sidebar_inset = (content.x0 - self.rect.x0).max(0.0);
        let beside = self.chrome.inspector.resolve(self.rect);
        let inspector_inset = (self.rect.x1 - beside.x1).max(0.0);
        self.refresh_toolbar();
        // The layout tree points at the People buttons, and the framework
        // syncs again before it measures; rebuilding them only here keeps
        // those pointers alive until the next tree replaces them.
        self.refresh_people_controls();

        // The painted header still needs admitted layout owners for its
        // retained controls: accessibility invokes their actual action doors.
        let mut header_controls = vec![
            self.chrome.search.build_layout(taffy, theme, node_map),
            self.chrome.toolbar.build_layout(taffy, theme, node_map),
        ];
        if self.header_back_visible() {
            header_controls.push(self.chrome.back.build_layout(taffy, theme, node_map));
        }
        if let Some(history) = self.chrome.history.button_mut() {
            header_controls.push(history.build_layout(taffy, theme, node_map));
        }
        for &control in &header_controls {
            if let Ok(style) = taffy.style(control) {
                let mut style = style.clone();
                style.position = taffy::Position::Absolute;
                let _ = taffy.set_style(control, style);
            }
        }
        let header = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(chrome::header_height(theme) as f32),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                &header_controls,
            )
            .unwrap();
        let canvas = self.build_canvas_node(taffy, theme, node_map);
        let status = self.build_status_node(taffy, theme);

        let style = Style {
            flex_direction: FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            padding: taffy::Rect {
                left: LengthPercentage::length(sidebar_inset as f32),
                right: LengthPercentage::length(inspector_inset as f32),
                top: LengthPercentage::length(0.0),
                bottom: LengthPercentage::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[header, canvas, status], node_map, self)
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
        let child_rect = |child: NodeId| {
            let l = taffy.layout(child).unwrap();
            Rect::new(
                x + f64::from(l.location.x),
                y + f64::from(l.location.y),
                x + f64::from(l.location.x) + f64::from(l.size.width),
                y + f64::from(l.location.y) + f64::from(l.size.height),
            )
        };

        let children = taffy.children(node).unwrap();
        if let Some(&header) = children.first() {
            self.chrome.header = child_rect(header);
        }
        if let Some(&canvas) = children.get(1) {
            self.canvas_rect = child_rect(canvas);
            let canvas_origin = Point::new(self.canvas_rect.x0, self.canvas_rect.y0);
            let people_surface = self.library_mode
                && (self.library_destination == LibraryDestination::People || self.people_mode);
            if people_surface {
                let canvas_children = taffy.children(canvas).unwrap_or_default();
                let show_name = self.people_selection.is_some() && !self.people_merge_mode;
                let mut index = 0usize;
                if show_name {
                    if let Some(&field) = canvas_children.get(index) {
                        self.people_name_input
                            .apply_layout(taffy, field, canvas_origin);
                    }
                    index += 1;
                }
                for control in &mut self.people_controls {
                    if let Some(&button) = canvas_children.get(index) {
                        control.button.apply_layout(taffy, button, canvas_origin);
                        if !control.is_card() {
                            control.width = control.button.get_rect().width();
                        }
                    }
                    index += 1;
                }
                // The tree was placed from the canvas the previous frame
                // had; place the surface again on the canvas it has now, so
                // the painted headings and the controls always agree.
                let theme = self.layout_theme.clone();
                self.layout_people_controls(&theme);
                if show_name {
                    self.people_name_input
                        .apply_layout_external(self.people_name_bounds);
                }
                for control in &mut self.people_controls {
                    let bounds = if control.is_card() {
                        Rect::from_origin_size(canvas_origin, (0.0, 0.0))
                    } else {
                        control.bounds
                    };
                    control.button.apply_layout_external(bounds);
                }
            } else if let Some(empty) = &mut self.empty_state {
                empty.apply_layout(taffy, canvas, here);
            } else if let Some(v) = self.viewer.as_mut() {
                v.apply_layout(taffy, canvas, here);
            }
        }
        if let Some(&status) = children.get(2) {
            self.status_rect = child_rect(status);
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
        // No window ground: the framework paints `surfaces.content` under
        // the app, and the sidebar paints its own region.
        self.chrome.drawer.tick(theme);
        self.chrome.drawer.resolve(self.rect);
        self.chrome.inspector.tick(theme);
        self.chrome.inspector.resolve(self.rect);
        self.place_chrome(text_renderer, theme);

        if self.library_mode {
            if self.library_destination == LibraryDestination::Albums {
                self.draw_albums(scene, transform, text_renderer, theme);
            } else if self.library_destination == LibraryDestination::People || self.people_mode {
                self.draw_people(scene, transform, text_renderer, theme);
                if self.people_selection.is_some() && !self.people_merge_mode {
                    self.people_name_input
                        .draw(scene, transform, text_renderer, theme, presence);
                }
                for control in &mut self.people_controls {
                    if !control.is_card() {
                        control
                            .button
                            .draw(scene, transform, text_renderer, theme, presence);
                    }
                }
            } else if self.library_destination == LibraryDestination::Places {
                self.draw_places(scene, transform, text_renderer, theme, presence);
            } else if self.library_destination == LibraryDestination::Analysis {
                self.draw_library_analysis(scene, transform, text_renderer, theme, presence);
            } else {
                self.draw_library(scene, transform, text_renderer, theme);
            }
        } else if let Some(v) = self.viewer.as_mut() {
            v.draw(scene, transform, text_renderer, theme, presence);
            self.draw_viewer_details(scene, transform, text_renderer, theme);
        } else {
            let (title, body) = self.empty_state_text();
            self.draw_empty_state(scene, transform, text_renderer, theme, &title, &body);
        }

        self.draw_directory_import_status(scene, transform, text_renderer, theme);
        self.paint_status(scene, transform, text_renderer, theme);
        self.paint_header(scene, transform, text_renderer, theme, presence);
        // The inspector comes after the header: floating in a narrow window
        // it stands over the header row, never under it.
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
        if self.library_mode
            && !self.people_mode
            && self.library_destination == LibraryDestination::Timeline
        {
            self.draw_metadata_editor(scene, transform, text_renderer, theme, presence);
        }
        let now = ui::time::now();
        if let Some(notice) = self.notice.as_deref() {
            self.status_bubble.show(notice, now);
        }
        self.status_bubble
            .pointer_moved(self.canvas_rect, self.pointer, text_renderer, theme, now);
        self.status_bubble.paint(
            scene,
            transform,
            text_renderer,
            theme,
            self.canvas_rect,
            now,
        );
        self.paint_sidebar(scene, transform, text_renderer, theme);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.chrome
            .toolbar
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        if self.library_mode && self.selection_album_chooser {
            self.draw_selection_destination_sheet(scene, transform, text_renderer, theme, presence);
        }
        self.draw_directory_import_details(scene, transform, text_renderer, theme, presence);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event entry point preserves explicit precedence between album chips, grid cards, toolbar, and viewer"
    )]
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.reconcile_content();
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            let next = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            if self.library_mode
                && self.library_destination == LibraryDestination::Places
                && let Some(previous) = self.places_drag_origin.replace(next)
            {
                self.set_places_viewport(self.places_viewport.panned(
                    next.x - previous.x,
                    next.y - previous.y,
                    self.places_map_bounds.width(),
                    self.places_map_bounds.height(),
                ));
            }
            self.pointer = next;
        }
        if self.directory_import_details_open {
            let theme = self.layout_theme.clone();
            self.directory_import_details_scroll.set_focused(true);
            self.directory_import_details_scroll
                .handle_event(event, ctx, &theme);
            if *ctx.handled {
                return;
            }
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == winit::event::ElementState::Pressed
            {
                match &key.logical_key {
                    Key::Named(NamedKey::Escape | NamedKey::Enter) => {
                        self.close_directory_details();
                    }
                    _ => return,
                }
                *ctx.handled = true;
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
                if let Some(layout) = self.directory_import_details_layout.clone() {
                    match layout.hit(self.pointer) {
                        Some(ModalHit::Close | ModalHit::Scrim | ModalHit::Action(0)) => {
                            self.close_directory_details();
                        }
                        Some(ModalHit::Card | ModalHit::Action(_)) | None => {}
                    }
                }
                *ctx.handled = true;
                return;
            }
            // A modal swallows pointer/keyboard input that it does not use;
            // cursor motion still reaches it so hover feedback remains live.
            if !matches!(event, winit::event::WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
                return;
            }
        }
        if self.library_mode && self.selection_album_chooser {
            let theme = self.layout_theme.clone();
            self.selection_album_scroll.set_focused(true);
            self.selection_album_scroll.handle_event(event, ctx, &theme);
            if *ctx.handled {
                return;
            }
        }
        if !self.selection_album_chooser && self.handle_chrome_event(event, ctx) {
            self.drain_pending();
            return;
        }
        if let Some(empty) = &mut self.empty_state {
            empty.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        for (_, button) in &mut self.directory_import_buttons {
            let mut sink = None;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    focus_target: &mut sink,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
        }
        if self.directory_import_pending.get().is_none()
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .directory_import_buttons
                .iter()
                .find(|(_, button)| button.is_enabled() && button.get_rect().contains(self.pointer))
        {
            self.directory_import_pending.set(Some(*hit));
        }
        if let Some(hit) = self.directory_import_pending.take() {
            if self.activate_directory_status(hit) {
                *ctx.handled = true;
            }
            return;
        }
        if *ctx.handled {
            return;
        }
        let people_surface = self.library_mode
            && (self.library_destination == LibraryDestination::People || self.people_mode);
        if people_surface {
            if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
                && key.state == winit::event::ElementState::Pressed
                && matches!(key.logical_key, Key::Named(NamedKey::Escape))
            {
                self.handle_people_intent(PeopleIntent::Back);
                *ctx.handled = true;
                return;
            }
            if let winit::event::WindowEvent::MouseWheel { delta, .. } = event
                && self.people_selection.is_none()
                && self.people_layout.lanes.contains(self.pointer)
            {
                let theme = self.layout_theme.clone();
                self.people_scroll += wheel_pixels(delta, &theme);
                self.layout_people_controls(&theme);
                *ctx.handled = true;
                return;
            }
            let root_children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
            if let Some(&canvas) = root_children.get(1) {
                let canvas_children = ctx.taffy.children(canvas).unwrap_or_default();
                let show_name = self.people_selection.is_some() && !self.people_merge_mode;
                let mut index = 0usize;
                if show_name {
                    if let Some(&field) = canvas_children.get(index) {
                        self.people_name_input
                            .handle_event(event, &mut ctx.reborrow(ctx.path, field));
                    }
                    index += 1;
                    if self.people_name_input.take_submit() {
                        let intent = match self.people_selection {
                            Some(PeopleSelection::Approved(_)) => PeopleIntent::SaveName,
                            Some(PeopleSelection::Suggestion(_)) => PeopleIntent::ApproveSuggestion,
                            None => PeopleIntent::Back,
                        };
                        self.handle_people_intent(intent);
                        *ctx.handled = true;
                        return;
                    }
                }
                for control in &mut self.people_controls {
                    if let Some(&button) = canvas_children.get(index) {
                        control
                            .button
                            .handle_event(event, &mut ctx.reborrow(ctx.path, button));
                    }
                    index += 1;
                }
            }
            let mut pending = self.people_pending.borrow_mut().take();
            if pending.is_none()
                && matches!(
                    event,
                    winit::event::WindowEvent::MouseInput {
                        state: winit::event::ElementState::Released,
                        button: winit::event::MouseButton::Left,
                        ..
                    }
                )
            {
                let lanes = self.people_layout.lanes;
                pending = self
                    .people_controls
                    .iter()
                    .find(|control| {
                        control.is_card()
                            && control.bounds.contains(self.pointer)
                            && lanes.contains(self.pointer)
                    })
                    .map(|control| control.intent.clone());
            }
            if let Some(intent) = pending {
                self.handle_people_intent(intent);
                *ctx.handled = true;
                return;
            }
            if *ctx.handled {
                return;
            }
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
            *ctx.handled = true;
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
        if self.library_mode && self.library_destination == LibraryDestination::Analysis {
            for (_, button) in &mut self.library_analysis_buttons {
                let mut sink = None;
                button.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        focus_target: &mut sink,
                        ..ctx.reborrow(ctx.path, ctx.node_id)
                    },
                );
                if sink.is_some() {
                    *ctx.focus_target = sink;
                }
            }
            if let Some(hit) = self.library_analysis_pending.take() {
                self.activate_library_analysis(hit);
                *ctx.handled = true;
                return;
            }
            if matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            ) && let Some((photo_id, _)) = self
                .library_analysis_failure_hits
                .iter()
                .find(|(_, bounds)| bounds.contains(self.pointer))
                .cloned()
            {
                self.show_photo(&photo_id);
                *ctx.handled = true;
                return;
            }
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Albums
            && !self.people_mode
            && self.albums_viewport.contains(self.pointer)
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            self.albums_scroll += wheel_pixels(delta, &self.layout_theme);
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                winit::event::MouseScrollDelta::PixelDelta(point) => point.y / 80.0,
            };
            self.zoom_places_by(amount * 0.25);
            *ctx.handled = true;
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
                PlacesHit::Place(key) => {
                    let _ = self.open_place_collection(&self.presented(), &key);
                }
                PlacesHit::Photo(id) => {
                    self.show_photo(&id);
                }
            }
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Places
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::ArrowLeft) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        44.0,
                        0.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        -44.0,
                        0.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        0.0,
                        44.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.set_places_viewport(self.places_viewport.panned(
                        0.0,
                        -44.0,
                        self.places_map_bounds.width(),
                        self.places_map_bounds.height(),
                    ));
                }
                Key::Character(key) if key == "+" || key == "=" => self.zoom_places_by(1.0),
                Key::Character(key) if key == "-" => self.zoom_places_by(-1.0),
                Key::Character(key) if key.eq_ignore_ascii_case("l") => {
                    self.places_list_mode = !self.places_list_mode;
                }
                _ => return,
            }
            *ctx.handled = true;
            return;
        }

        // Give the shared chronological index first refusal only while its
        // narrow track/preview lane owns the pointer (or it holds a drag / AT
        // focus). Ordinary scrolling anywhere else remains the timeline's
        // smooth wheel path below.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
        {
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
                    ctx.scale_factor,
                    self.timeline_index_metrics,
                    &self.timeline_index_markers,
                ) {
                    let _ = self.apply_timeline_index_intent(&intent);
                    *ctx.handled = true;
                    return;
                }
                if was_dragging || self.timeline_index.is_dragging() {
                    *ctx.handled = true;
                    return;
                }
            }
        }

        // Ctrl + wheel is the cross-platform density gesture.  Keep an
        // ordinary wheel for chronological browsing; otherwise a trackpad
        // scroll would unexpectedly rearrange the entire library.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
            && ctx.modifiers.state().control_key()
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let amount = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => *y,
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y as f32,
            };
            if amount != 0.0 {
                // Natural scroll semantics: up means see more at once.
                self.adjust_timeline_density(amount < 0.0);
                *ctx.handled = true;
                return;
            }
        }

        // Tilo's vendored Wayland winit maps zwp_pointer_gestures_v1 to this
        // same event; macOS/iOS use it natively.  Ctrl-wheel above remains the
        // fallback on compositors that do not advertise pinch gestures.
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::PinchGesture { delta, .. } = event
            && delta.is_finite()
            && *delta != 0.0
        {
            self.adjust_timeline_density(*delta > 0.0);
            *ctx.handled = true;
            return;
        }

        if self.library_mode && self.handle_metadata_editor_event(event, ctx) {
            return;
        }

        if self.library_mode && self.handle_album_editor_event(event, ctx) {
            return;
        }

        if self.handle_collection_page_event(event, ctx) {
            return;
        }

        if self.library_mode
            && !self.album_editor
            && !self.metadata_editor
            && let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value.eq_ignore_ascii_case("a"))
                && self.selection_mode
            {
                self.select_all_visible();
                *ctx.handled = true;
                return;
            }
            if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                && matches!(&key.logical_key, Key::Character(value) if value == " ")
            {
                if self.selection_mode {
                    self.finish_selection();
                } else {
                    self.begin_selection();
                }
                *ctx.handled = true;
                return;
            }
            if self.selection_mode && matches!(&key.logical_key, Key::Named(NamedKey::Escape)) {
                self.finish_selection();
                *ctx.handled = true;
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
                *ctx.handled = true;
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
            && let Some((album_id, _)) = self
                .album_edit_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = Some(album_id);
            self.begin_edit_album();
            *ctx.handled = true;
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
            *ctx.handled = true;
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
            && self
                .selection_album_scroll
                .content_rect()
                .contains(self.pointer)
            && let Some((album_id, _)) = self
                .selection_destination_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.selection_destination_album = Some(album_id);
            self.notice = Some(tr("photos-selection-album-chosen"));
            *ctx.handled = true;
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
            self.handle_selection_hit(hit);
            *ctx.handled = true;
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
            *ctx.handled = true;
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
                AlbumEditorHit::New => self.begin_new_album(),
                AlbumEditorHit::Edit => self.begin_edit_album(),
                _ => {}
            }
            *ctx.handled = true;
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
            self.library_destination = LibraryDestination::Timeline;
            self.active_folder = None;
            self.active_person = None;
            self.active_tag = None;
            self.active_favorites = false;
            self.library_offset = 0;
            *ctx.handled = true;
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
                let _ = self.view_favorites(&self.presented());
            } else {
                self.active_album = None;
                self.active_folder = card.folder;
                self.active_person = card.person;
                self.active_tag = card.tag;
                self.active_favorites = false;
                self.library_offset = 0;
                self.timeline_scroll_y = 0.0;
                self.notice = None;
            }
            *ctx.handled = true;
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
            self.begin_metadata_editor(&id);
            *ctx.handled = true;
            return;
        }
        if self.library_mode
            && self.library_destination == LibraryDestination::Timeline
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let pixels = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 48.0,
                winit::event::MouseScrollDelta::PixelDelta(position) => -position.y,
            };
            self.scroll_timeline_pixels(pixels);
            *ctx.handled = true;
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
                self.toggle_library_selection(&photo_id, ctx.modifiers.state().shift_key());
            } else if !self.album_editor
                && card_action_controls(card, &self.layout_theme)
                    .favorite
                    .union(card_action_controls(card, &self.layout_theme).remove)
                    .contains(self.pointer)
            {
                let actions = card_action_controls(card, &self.layout_theme);
                if actions.favorite.contains(self.pointer) {
                    self.library_remove_confirm = None;
                    self.commit(crate::model::photo_paths(&photo_id).favorite.set(!favorite));
                } else if self.library_remove_confirm.as_deref() == Some(&photo_id) {
                    self.commit(PhotoLibrary::REMOVE_PHOTO.with(Id::from_raw(photo_id)));
                    self.library_remove_confirm = None;
                    self.notice = Some(tr("photos-removed"));
                } else {
                    self.library_remove_confirm = Some(photo_id);
                    self.notice = Some(tr("photos-remove-confirm"));
                }
            } else {
                self.library_remove_confirm = None;
                self.show_photo(&photo_id);
            }
            *ctx.handled = true;
            return;
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if let (Some(v), Some(&canvas)) = (self.viewer.as_mut(), children.get(1)) {
            v.handle_event(event, &mut ctx.reborrow(ctx.path, canvas));
        }
        self.drain_pending();
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
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self
            .empty_state
            .as_mut()
            .is_some_and(|empty| empty.perform_semantic_action(target, action, state, path))
        {
            return true;
        }
        if self.perform_chrome_semantic_action(target, action, state, path) {
            return true;
        }
        let (state, _) = ui::doc::document(state, path);
        self.reconcile_content();
        self.perform_directory_semantic_action(target, action)
            || self.perform_library_semantic_action(target, action, state)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.canvas_rect),
            _ => None,
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl PhotosApp {
    /// Why the last search refresh was rejected, while the last good index
    /// stays in use: a row a caller can read and act on.
    pub(crate) fn search_status_row(&self) -> Option<ui::framework::LiveRow> {
        let message = self.search_refresh_error.as_deref()?;
        Some(
            ui::framework::LiveRow::new(
                "photos.search-status",
                "photos.diagnostic",
                "Photo search status",
            )
            .row(ui::framework::LiveRow::new(
                "photos.search-status#message",
                "photos.status",
                message,
            ))
            .effect(&PhotosEffect::ViewLibrary),
        )
    }
}

impl ui::widget::controls::ActionControls for PhotosApp {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        ui::widget::controls::install_control_custody(&mut self.chrome.toolbar, domain, attachment);
        for button in self.retained_buttons_mut() {
            ui::widget::controls::install_control_custody(button, domain, attachment);
        }
        if let Some(empty) = &mut self.empty_state {
            ui::widget::controls::install_control_custody(empty, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.chrome
            .search_opener
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(&mut self.chrome.toolbar))
            .or_else(|| {
                self.empty_state
                    .as_mut()
                    .and_then(|empty| ui::widget::controls::take_control_invocation(empty))
            })
            .or_else(|| {
                self.retained_buttons_mut()
                    .into_iter()
                    .find_map(|button| ui::widget::controls::take_control_invocation(button))
            })
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        // Photos projects its custom media surfaces and retained buttons into
        // one complete semantic tree above. Inventory that visible tree once:
        // appending every retained widget here also advertises controls that
        // are currently hidden (other routes, closed sheets, disabled jobs),
        // producing orphan automation bindings with no matching UI target.
        // The buttons' canonical resources remain published by `resources`
        // and are dispatched through the same retained widgets.
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        bindings.extend(ui::widget::controls::control_action_bindings(
            &self.chrome.toolbar,
        ));
        if self.header_back_visible() {
            bindings.extend(ui::widget::controls::control_action_bindings(
                &self.chrome.back,
            ));
        }
        bindings.extend(ui::widget::controls::control_action_bindings(
            &self.chrome.search,
        ));
        if let Some(empty) = &self.empty_state {
            bindings.extend(ui::widget::controls::control_action_bindings(empty));
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        ui::widget::controls::refresh_control_action_availability(
            &mut self.chrome.toolbar,
            allowed,
        );
        for button in self.retained_buttons_mut() {
            button.refresh_control_action_availability(allowed);
        }
        if let Some(empty) = &mut self.empty_state {
            ui::widget::controls::refresh_control_action_availability(empty, allowed);
        }
    }
}

impl ui::widget::WindowHeader for PhotosApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        self.chrome.history.set_control(control);
        self.structure_dirty |= self.chrome.history.take_rebuilt();
    }
}

impl PhotosApp {
    /// Answer `effect` — invoked by a control, the palette, an agent or a
    /// script — against `library` as it is now. `Ok(None)` when its work
    /// continues: it reports through [`Self::settle`] under `invocation`
    /// and changes the library with ordinary actions as it goes.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match over every Photos effect keeps their answers auditable"
    )]
    pub(crate) fn answer_effect(
        &mut self,
        effect: PhotosEffect,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let done = || Ok(Some(tilo_actions::ActionOutput::default()));
        if matches!(
            effect,
            PhotosEffect::RotateShownLeft
                | PhotosEffect::RotateShownRight
                | PhotosEffect::CropShownToView
                | PhotosEffect::ResetShownEdits
                | PhotosEffect::ShareShown
        ) && self.shown_photo().is_none()
        {
            return Err(tr("photos-empty-title"));
        }
        if self.post_control(&effect) {
            return done();
        }
        match effect {
            PhotosEffect::View { photo_id } => self.view_photo(library, &photo_id)?,
            PhotosEffect::ViewLibrary => self.show_library(),
            PhotosEffect::ViewAlbums => self.show_albums(),
            PhotosEffect::ViewTimeline { anchor } => {
                self.view_timeline(library, anchor.as_ref())?;
            }
            PhotosEffect::ViewPeople => {
                self.show_people();
                self.notice = Some(
                    "People is local-first: install a signed face model, then review every suggestion before sharing labels".to_owned(),
                );
            }
            PhotosEffect::ViewLibraryAnalysis => {
                self.show_library_analysis();
            }
            PhotosEffect::ManagePeopleRecognition => {
                self.notice = Some(
                    "Open Models in Settings to install and start a signed local face-recognition model"
                        .to_owned(),
                );
            }
            PhotosEffect::EnablePeopleRecognition => {
                self.people_controller.set_recognition_enabled(true);
                self.people_controller
                    .discover_face_capability(&agent_provider_config::ProviderConfig::load());
                self.notice = Some(self.people_controller.capability().summary());
            }
            PhotosEffect::ViewFavorites => self.view_favorites(library)?,
            PhotosEffect::ViewPlaces => {
                self.show_library();
                self.library_destination = LibraryDestination::Places;
                self.places_open_key = None;
                self.places_fit_pending = true;
            }
            PhotosEffect::ViewPlace { place_key } => {
                self.open_place_collection(library, &place_key)?;
            }
            PhotosEffect::ViewAlbum { album_id } => self.view_album(library, &album_id)?,
            PhotosEffect::ViewFolder { folder } => {
                self.view_metadata_filter(library, Some(folder), None, None)?;
            }
            PhotosEffect::ViewPerson { person } => {
                self.view_metadata_filter(library, None, Some(person), None)?;
            }
            PhotosEffect::ViewTag { tag } => {
                self.view_metadata_filter(library, None, None, Some(tag))?;
            }
            PhotosEffect::ViewObject { object } => {
                self.view_analysis_filter(library, object, false)?;
            }
            PhotosEffect::ViewPet { pet } => self.view_analysis_filter(library, pet, true)?,
            PhotosEffect::AnalyzeLibrary => {
                self.begin_library_analysis(invocation, library, false)?;
                return Ok(None);
            }
            PhotosEffect::ResumeLibraryAnalysis => {
                self.resume_library_analysis(invocation, library)?;
                return Ok(None);
            }
            PhotosEffect::RetryLibraryAnalysisFailures => {
                self.begin_library_analysis(invocation, library, true)?;
                return Ok(None);
            }
            PhotosEffect::PauseLibraryAnalysis => {
                if !self.pause_library_analysis() {
                    return Err("Photo analysis is not running".to_owned());
                }
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "completed": self.library_analysis.completed,
                        "remaining": self.library_analysis.remaining(),
                    })),
                    resources: vec!["photos.library-analysis".to_owned()],
                }));
            }
            PhotosEffect::AnalyzeText {
                resource_id,
                language,
            } => {
                let photo = library
                    .photo(&resource_id)
                    .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
                if photo.image.content.is_none() {
                    return Err("This photograph needs a verified content identity before text recognition can be shared".to_owned());
                }
                let sender = self.analysis_sender().ok_or_else(|| {
                    "local text recognition is unavailable on this device".to_owned()
                })?;
                sender
                    .try_send(AnalysisJob {
                        invocation,
                        photo_id: resource_id,
                        content: photo.image.content.clone(),
                        image: photo.image.clone(),
                        kind: AnalysisJobKind::Ocr { language },
                    })
                    .map_err(|_| {
                        "text recognition is busy; wait for the current photos to finish".to_owned()
                    })?;
                self.active_analysis.insert(invocation);
                return Ok(None);
            }
            PhotosEffect::AnalyzeImage { resource_id } => {
                let photo = library
                    .photo(&resource_id)
                    .ok_or_else(|| "that photograph is no longer in the library".to_owned())?;
                if photo.image.content.is_none() {
                    return Err("This photograph needs a verified content identity before image analysis can be shared".to_owned());
                }
                let capability = tilo_vision::discover_local_capability(
                    &agent_provider_config::ProviderConfig::load(),
                )
                .ok_or_else(|| {
                    "No verified local image-analysis model is ready. Install and start one in Settings."
                        .to_owned()
                })?;
                let sender = self.analysis_sender().ok_or_else(|| {
                    "local image analysis is unavailable on this device".to_owned()
                })?;
                sender
                    .try_send(AnalysisJob {
                        invocation,
                        photo_id: resource_id,
                        content: photo.image.content.clone(),
                        image: photo.image.clone(),
                        kind: AnalysisJobKind::Vision { capability },
                    })
                    .map_err(|_| {
                        "local image analysis is busy; wait for the current photos to finish"
                            .to_owned()
                    })?;
                self.active_analysis.insert(invocation);
                return Ok(None);
            }
            PhotosEffect::AnalyzePeopleLibrary => {
                self.schedule_people_scan_batch(invocation, library)?;
                return Ok(None);
            }
            PhotosEffect::ImportCameraMedia { media_id, name } => {
                // Verify the portable Camera identity and bytes before the
                // one idempotent library import: a missing local blob is a
                // refusal, never a phantom entry.
                let photo = crate::camera_import_photo(&media_id, &name, &self.content)?;
                let id = photo.id.clone();
                let dispatch = self
                    .dispatch
                    .as_ref()
                    .ok_or_else(|| "this window edits no library".to_owned())?;
                dispatch.submit(crate::resource::import(&photo));
                self.notice = Some("Imported Camera photo".to_owned());
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::Value::String(id.clone())),
                    resources: vec![id],
                }));
            }
            PhotosEffect::ExportEditedContent { photo } => {
                return crate::actions::export_edited_content(
                    photo.as_str(),
                    library,
                    &self.content,
                )
                .map(Some);
            }
            PhotosEffect::Import { path: Some(file) } => {
                self.import_content = Some(LiveContent {
                    library: library.clone(),
                });
                self.pending_resource_open = Some((invocation, file));
                return Ok(None);
            }
            PhotosEffect::Save { path } => {
                let open = self
                    .shown_photo_in(library)
                    .ok_or_else(|| "no photograph is open".to_string())?;
                let destination = path.unwrap_or_else(|| Self::suggested_save_path(&open.image));
                if !self.save_to(&destination) {
                    return Err(tr("photos-save-failed"));
                }
            }
            other => {
                self.perform_effect(other, library)?;
                if self.picker.is_some()
                    || self.contents_picker.is_some()
                    || self.directory_picker.is_some()
                    || self.directory_import.is_some()
                {
                    self.resource_invocation = Some(invocation);
                    if let Some(import) = self.directory_import.as_mut() {
                        import.invocation = Some(invocation);
                    }
                    return Ok(None);
                }
            }
        }
        done()
    }

    /// Keep `reply`, owed to the caller of the effect whose work reports
    /// under `invocation`.
    pub(crate) fn register_reply(
        &mut self,
        invocation: tilo_actions::InvocationId,
        reply: ui::framework::EffectReply,
    ) {
        self.replies.insert(invocation, reply);
    }

    /// Answer the caller of `invocation` now.
    pub(crate) fn settle_now(
        &mut self,
        invocation: tilo_actions::InvocationId,
        result: Result<tilo_actions::ActionOutput, String>,
    ) {
        if let Some(reply) = self.replies.remove(&invocation) {
            reply.finish(result);
        }
    }

    /// Drop the reply owed under `invocation`: the effect failed at once,
    /// and its caller learns that from the failure itself.
    pub(crate) fn forget_reply(&mut self, invocation: tilo_actions::InvocationId) {
        self.replies.remove(&invocation);
    }

    /// What the job answering `update`'s invocation reports: its progress,
    /// or its end, told to whoever invoked it.
    fn settle(&mut self, update: tilo_actions::InvocationUpdate) {
        let invocation = update.invocation();
        #[cfg(test)]
        self.settled.push(update.clone());
        match update {
            tilo_actions::InvocationUpdate::Progress {
                phase,
                completed,
                total,
                ..
            } => {
                if let Some(reply) = self.replies.get(&invocation) {
                    reply.progress().report(phase, completed, total);
                }
            }
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                if let Some(reply) = self.replies.remove(&invocation) {
                    reply.finish(Ok(output));
                }
            }
            tilo_actions::InvocationUpdate::Failed { error, .. } => {
                if let Some(reply) = self.replies.remove(&invocation) {
                    reply.answer(Err(error));
                }
            }
            tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
                if let Some(reply) = self.replies.remove(&invocation) {
                    reply.finish(Err(message.unwrap_or_else(|| "cancelled".to_owned())));
                }
            }
            _ => {}
        }
    }

    /// The last report settled for `invocation`, taken.
    #[cfg(test)]
    pub(crate) fn take_settled(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let at = self
            .settled
            .iter()
            .rposition(|update| update.invocation() == invocation)?;
        Some(self.settled.remove(at))
    }

    /// Stop the work its callers cancelled. The framework has told them;
    /// what is stopped here is the work itself.
    fn follow_cancellations(&mut self) {
        let cancelled: Vec<_> = self
            .replies
            .iter()
            .filter(|(_, reply)| reply.is_cancelled())
            .map(|(invocation, _)| *invocation)
            .collect();
        for invocation in cancelled {
            self.replies.remove(&invocation);
            self.cancel_invocation(invocation);
        }
    }

    /// Stop the work answering `invocation`.
    fn cancel_invocation(&mut self, invocation: tilo_actions::InvocationId) {
        if self.library_analysis.is_running_invocation(invocation) {
            // The photograph in progress may finish safely.
            self.pause_library_analysis();
        }
        if self.active_analysis.remove(&invocation) {
            self.cancelled_analysis.insert(invocation);
            if self.active_people_scans.remove(&invocation) {
                self.pending_people_scans.remove(&invocation);
                "cancelled".clone_into(&mut self.people_scan_progress.status);
                self.people_scan_progress.save();
            }
        }
        if self
            .pending_resource_open
            .as_ref()
            .is_some_and(|(pending, _)| *pending == invocation)
        {
            self.pending_resource_open = None;
        }
        if self.resource_invocation == Some(invocation) {
            // Dropping the receiver closes this app's portal request channel;
            // a late response can no longer open a file after the caller
            // cancelled. Completed photographs of a folder are kept.
            self.picker = None;
            self.contents_picker = None;
            self.directory_picker = None;
            if let Some(import) = self.directory_import.as_mut() {
                import.cancel.store(true, Ordering::Relaxed);
                import.invocation = None;
                import.cancelled = true;
                import.pending.clear();
            }
            self.resource_invocation = None;
        }
    }
}

impl PhotosApp {
    /// Admit one fresh contiguous People slice plus a bounded, device-local
    /// anchor set from earlier local suggestions. Anchors use ordinary local
    /// pixels only to let the signed runtime bridge disjoint batches; they do
    /// not count toward cursor progress and never leave this device.
    fn schedule_people_scan_batch(
        &mut self,
        invocation: tilo_actions::InvocationId,
        library: &crate::model::PhotoLibrary,
    ) -> Result<(), String> {
        if !self.people_controller.privacy().recognition_enabled {
            return Err(tr("photos-people-error-off"));
        }
        let config = agent_provider_config::ProviderConfig::load();
        self.people_controller.discover_face_capability(&config);
        let capability = tilo_vision::discover_local_face_capability(&config).ok_or_else(|| {
            "No verified local face model is ready. Install and start one in Settings.".to_owned()
        })?;
        let locally_available: Vec<_> = library
            .photos
            .values()
            .filter(|photo| crate::model::bytes_of(&photo.image, &self.content).is_some())
            .cloned()
            .map(|photo| (photo.id, photo.image))
            .collect();
        let anchor_ids = self.people_controller.reconciliation_anchor_photo_ids(2);
        let anchor_count = anchor_ids
            .iter()
            .filter(|id| {
                locally_available
                    .iter()
                    .any(|(available, _)| available == *id)
            })
            .count()
            .min(tilo_vision::MAX_FACE_BATCH.saturating_sub(1));
        let (start, scan_len) = self
            .people_scan_progress
            .batch_range(
                locally_available.len(),
                tilo_vision::MAX_FACE_BATCH.saturating_sub(anchor_count),
            )
            .ok_or_else(|| tr("photos-people-error-no-photos"))?;
        let mut photos: Vec<_> = locally_available
            .iter()
            .skip(start)
            .take(scan_len)
            .cloned()
            .collect();
        for id in anchor_ids {
            if photos.iter().any(|(photo_id, _)| photo_id == &id) {
                continue;
            }
            if let Some(anchor) = locally_available
                .iter()
                .find(|(photo_id, _)| photo_id == &id)
            {
                photos.push(anchor.clone());
            }
        }
        let Some((_, first)) = photos.first() else {
            return Err(tr("photos-people-error-no-photos"));
        };
        let sender = self
            .analysis_sender()
            .ok_or_else(|| "local People recognition is unavailable on this device".to_owned())?;
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
            .map_err(|_| tr("photos-analysis-error-busy"))?;
        "running".clone_into(&mut self.people_scan_progress.status);
        self.people_scan_progress.last_error = None;
        self.people_scan_progress.last_library_size = locally_available.len();
        self.people_scan_progress.save();
        self.active_analysis.insert(invocation);
        self.active_people_scans.insert(invocation);
        self.pending_people_scans.insert(
            invocation,
            PendingPeopleScan {
                start,
                scanned: scan_len,
                total: locally_available.len(),
            },
        );
        Ok(())
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
            .unwrap_or(&self.app_name)
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
#[path = "app/tests.rs"]
mod tests;

#[cfg(test)]
mod history_reconcile_tests {
    use super::*;

    use automerge::ROOT;

    /// Present `doc`'s library to `app` as the framework does after a change,
    /// then sync it.
    fn sync(app: &mut PhotosApp, doc: &mut AutoCommit) {
        present(app, doc);
        Widget::sync(app, doc, &ROOT);
        settle(app, doc);
    }

    /// Apply what `app` submitted to `doc`, as the host does after a pass,
    /// and show it the result.
    fn settle(app: &mut PhotosApp, doc: &mut AutoCommit) {
        let Some(dispatch) = app.dispatch.clone() else {
            return;
        };
        if !ui::framework::testing::resolve(&dispatch, doc)
            .expect("submitted edits apply")
            .is_empty()
        {
            present(app, doc);
            Widget::sync(app, doc, &ROOT);
        }
    }

    /// Present `doc`'s library to `app`, as the framework does after a change.
    fn present(app: &mut PhotosApp, doc: &AutoCommit) {
        let state =
            <PhotoLibrary as ui::tilo_resource::Resource>::read(doc).expect("the library reads");
        app.adopt_resource_state(&Rc::new(state));
    }

    fn test_app() -> PhotosApp {
        test_app_on(ContentStore::memory())
    }
    fn test_app_on(content: ContentStore) -> PhotosApp {
        PhotosApp::with_content(content)
            .with_dispatcher(ui::framework::testing::dispatcher("so.tilo.photos"))
    }
    fn restore(doc: &mut AutoCommit, selected: &[automerge::ChangeHash]) {
        ui::history_source::steps::restore(doc, selected, &[]).unwrap();
    }
    /// Change the document's content: every photograph removed.
    fn empty_library(doc: &mut AutoCommit) {
        let mut library = crate::test_doc::library(doc);
        library.photos.retain(|_, _| false);
        crate::test_doc::set_library(doc, &library);
    }
    fn setup() -> (PhotosApp, AutoCommit, String) {
        let mut doc = AutoCommit::new();
        let image = OpenImage::from_path("historical-missing.png", 4, 3);
        let id = crate::test_doc::put_image(&mut doc, &image);
        let mut app = test_app();
        app.show_photo(&id);
        sync(&mut app, &mut doc);
        (app, doc, id)
    }
    #[test]
    fn thumbnails_follow_photo_identity_and_visibility() {
        eprintln!(
            "phase newly_visible_timeline_photo_uses_immutable_thumbnail_outside_queued_window"
        );
        {
            let mut doc = AutoCommit::new();
            let mut library = crate::model::PhotoLibrary::default();
            for index in 0..80 {
                let mut photo = crate::model::LibraryPhoto::from_image(
                    OpenImage::from_path(&format!("missing-timeline-{index}.png"), 30, 20)
                        .with_content(format!("{index:064x}")),
                );
                photo.captured_at =
                    Some(format!("2026-{:02}-{:02}", 1 + index / 28, 1 + index % 28));
                library.add_photo(photo);
            }
            let target = library.photos.values().nth(2).unwrap().clone();
            crate::test_doc::set_library(&mut doc, &library);
            let mut app = test_app();
            app.show_library();
            app.canvas_rect = Rect::new(0.0, 0.0, 1_440.0, 900.0);
            sync(&mut app, &mut doc);
            assert!(!app.thumbnail_resource_keys.contains_key(&target.id));
            app.view_timeline(
                &crate::test_doc::library(&doc),
                Some(&crate::actions::TimelineAnchor::Day {
                    day: target.captured_at.clone().unwrap(),
                }),
            )
            .unwrap();
            let mut text = TextRenderer::new();
            let mut missing = Scene::new();
            app.draw_library(&mut missing, Affine::IDENTITY, &mut text, &Theme::dark());
            assert!(app.library_cards.iter().any(|(id, _, _)| id == &target.id));
            assert!(!app.thumbnail_resource_keys.contains_key(&target.id));
            app.library_thumbnails.insert(
                thumbnail_key(&target.image),
                to_peniko(&crate::shots_pattern(30, 20)),
            );
            let mut hydrated = Scene::new();
            app.draw_library(&mut hydrated, Affine::IDENTITY, &mut text, &Theme::dark());
            assert!(app.library_cards.iter().any(|(id, _, _)| id == &target.id));
            let images = |scene: &Scene| {
                scene
                    .encoding()
                    .resources
                    .patches
                    .iter()
                    .filter(|patch| matches!(patch, vello_encoding::Patch::Image { .. }))
                    .count()
            };
            assert_eq!(
                images(&hydrated),
                images(&missing) + 1,
                "the production painter records the cached immutable image even before it enters the queued thumbnail window"
            );
        }

        eprintln!("phase changed_photo_identity_invalidates_cached_thumbnail_and_late_old_result");
        {
            let (mut app, mut doc, id) = setup();
            let old = app.thumbnail_generation.unwrap();
            app.library_thumbnails
                .insert(id.clone(), to_peniko(&crate::shots_pattern(4, 3)));
            let mut lib = crate::test_doc::library(&doc);
            lib.photos.iter_mut().next().unwrap().1.image.content = Some("a".repeat(64));
            crate::test_doc::set_library(&mut doc, &lib);
            sync(&mut app, &mut doc);
            assert!(!app.library_thumbnails.contains_key(&id));
            app.accept_thumbnail_result(ThumbnailResult {
                generation: old,
                id: id.clone(),
                preview: Some(crate::shots_pattern(4, 3)),
            });
            assert!(!app.library_thumbnails.contains_key(&id));
        }
    }

    fn dispatch_unmatched_action(app: &mut PhotosApp, doc: &mut AutoCommit) {
        app.perform_semantic_action(
            "nonexistent",
            &AccessAction::Activate,
            &mut ui::widget::Edits::lend(doc),
            &ROOT,
        );
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per phase")]
    #[test]
    fn changed_targets_invalidate_metadata_and_album_drafts() {
        eprintln!(
            "phase changed_target_invalidates_metadata_draft_before_input_but_unrelated_change_preserves_it"
        );
        {
            let (mut app, mut doc, id) = setup();
            let mut lib = crate::test_doc::library(&doc);
            lib.photos.iter_mut().next().unwrap().1.tags =
                crate::model::labels(["restored".to_owned()]);
            crate::test_doc::set_library(&mut doc, &lib);
            let selected = doc.get_heads();
            lib.photos.iter_mut().next().unwrap().1.tags =
                crate::model::labels(["live".to_owned()]);
            crate::test_doc::set_library(&mut doc, &lib);
            sync(&mut app, &mut doc);
            app.begin_metadata_editor(&id);
            app.metadata_tags_input.set_text("unsaved draft");
            crate::test_doc::set_photo_edits(&mut doc, &id, Edits::default().rotated_right());
            sync(&mut app, &mut doc);
            assert!(app.metadata_editor);
            assert_eq!(app.metadata_tags_input.value(), "unsaved draft");
            restore(&mut doc, &selected);
            present(&mut app, &doc);
            present(&mut app, &doc);
            dispatch_unmatched_action(&mut app, &mut doc);
            assert!(
                !app.metadata_editor,
                "stale draft must detach before any new input"
            );
            app.save_metadata_editor();
            settle(&mut app, &mut doc);
            assert_eq!(
                crate::test_doc::library(&doc)
                    .photos
                    .values()
                    .next()
                    .unwrap()
                    .tags(),
                vec!["restored"]
            );
            app.begin_metadata_editor(&id);
            app.metadata_tags_input.set_text("next edit");
            app.save_metadata_editor();
            settle(&mut app, &mut doc);
            assert_eq!(
                crate::test_doc::library(&doc)
                    .photos
                    .values()
                    .next()
                    .unwrap()
                    .tags(),
                vec!["next edit"]
            );
        }

        eprintln!("phase changed_second_batch_photo_invalidates_the_whole_metadata_draft");
        {
            let (mut app, mut doc, first) = setup();
            let second = crate::model::LibraryPhoto::from_image(OpenImage::from_path(
                "second-missing.png",
                4,
                3,
            ));
            let second_id = second.id.clone();
            let mut library = crate::test_doc::library(&doc);
            library.add_photo(second);
            crate::test_doc::set_library(&mut doc, &library);
            sync(&mut app, &mut doc);
            app.begin_metadata_editor_for_ids(vec![first, second_id.clone()]);
            app.metadata_tags_input.set_text("unsaved batch draft");
            library.photos.iter_mut().nth(1).unwrap().1.tags =
                crate::model::labels(["remote edit".to_owned()]);
            crate::test_doc::set_library(&mut doc, &library);
            present(&mut app, &doc);
            dispatch_unmatched_action(&mut app, &mut doc);
            assert!(
                !app.metadata_editor,
                "a changed non-first batch target invalidates the draft before input"
            );
            app.save_metadata_editor();
            present(&mut app, &doc);
            assert_eq!(
                crate::test_doc::library(&doc)
                    .photo(&second_id)
                    .unwrap()
                    .tags(),
                vec!["remote edit"]
            );
        }

        eprintln!("phase changed_album_or_deleted_member_detaches_draft_and_cannot_resurrect_it");
        {
            let (mut app, mut doc, id) = setup();
            let selected = doc.get_heads();
            let mut lib = crate::test_doc::library(&doc);
            lib.add_album(crate::model::Album {
                id: "photos.album:membership".into(),
                name: "Original".into(),
                photo_ids: crate::model::labels([id]),
            });
            crate::test_doc::set_library(&mut doc, &lib);
            sync(&mut app, &mut doc);
            app.active_album = Some("photos.album:membership".into());
            app.begin_edit_album();
            app.album_name_input.set_text("stale draft");
            restore(&mut doc, &selected);
            present(&mut app, &doc);
            sync(&mut app, &mut doc);
            assert!(!app.album_editor);
            app.save_album_editor();
            settle(&mut app, &mut doc);
            assert!(crate::test_doc::library(&doc).albums.is_empty());
            app.begin_new_album();
            app.album_name_input.set_text("Next album");
            app.save_album_editor();
            settle(&mut app, &mut doc);
            assert_eq!(
                crate::test_doc::library(&doc)
                    .albums
                    .values()
                    .next()
                    .unwrap()
                    .name,
                "Next album"
            );
        }
    }
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn late_opens_imports_and_restores_settle_against_current_content() {
        eprintln!("phase pending_resource_open_cannot_overwrite_changed_content_and_settles_once");
        {
            let (mut app, mut doc, _) = setup();
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("import.png");
            std::fs::write(
                &path,
                pixels::encode_png(&crate::shots_pattern(4, 3)).unwrap(),
            )
            .unwrap();
            let invocation = tilo_actions::InvocationId::new();
            app.answer_effect(
                PhotosEffect::Import {
                    path: Some(path.to_string_lossy().into()),
                },
                invocation,
                &crate::test_doc::library(&doc),
            )
            .unwrap();
            empty_library(&mut doc);
            let heads = doc.get_heads();
            sync(&mut app, &mut doc);
            assert!(crate::test_doc::library(&doc).photos.is_empty());
            assert_eq!(doc.get_heads(), heads);
            assert!(matches!(
                app.take_settled(invocation),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            assert!(app.take_settled(invocation).is_none(), "it settles once");
        }

        eprintln!(
            "phase already_dispatched_picker_reports_actual_outcome_without_importing_stale_selection"
        );
        {
            for outcome in [0, 1, 2] {
                let (mut app, mut doc, _) = setup();
                let invocation = tilo_actions::InvocationId::new();
                let (tx, rx) = std::sync::mpsc::channel();
                app.picker = Some(rx);
                app.resource_invocation = Some(invocation);
                app.import_content = Some(LiveContent {
                    library: crate::test_doc::library(&doc),
                });
                empty_library(&mut doc);
                sync(&mut app, &mut doc);
                tx.send(match outcome {
                    0 => ui::portal::PortalOutcome::Opened(std::path::PathBuf::from("chosen.png")),
                    1 => ui::portal::PortalOutcome::Cancelled,
                    _ => ui::portal::PortalOutcome::Failed("actual picker failure".into()),
                })
                .unwrap();
                let heads = doc.get_heads();
                sync(&mut app, &mut doc);
                assert_eq!(doc.get_heads(), heads);
                assert!(crate::test_doc::library(&doc).photos.is_empty());
                match app.take_settled(invocation).unwrap() {
                    tilo_actions::InvocationUpdate::Failed {
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
                    tilo_actions::InvocationUpdate::Cancelled { invocation: id, .. } => {
                        assert_eq!(id, invocation);
                        assert_eq!(outcome, 1);
                    }
                    other => panic!("unexpected settlement {other:?}"),
                }
                assert!(app.take_settled(invocation).is_none());
            }
        }

        eprintln!("phase failed_import_reports_failure_even_when_previous_photo_is_still_decoded");
        {
            let mut doc = AutoCommit::new();
            let store = ContentStore::memory();
            let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
            let open =
                OpenImage::from_path("existing.png", 6, 4).with_content(store.put(&bytes).unwrap());
            let id = crate::test_doc::put_image(&mut doc, &open);
            let mut app = test_app_on(store);
            app.show_photo(&id);
            sync(&mut app, &mut doc);
            assert!(app.open_image_path().is_some());
            let invocation = tilo_actions::InvocationId::new();
            let tmp = tempfile::tempdir().unwrap();
            app.answer_effect(
                PhotosEffect::Import {
                    path: Some(tmp.path().join("absent.png").to_string_lossy().into()),
                },
                invocation,
                &crate::test_doc::library(&doc),
            )
            .unwrap();
            sync(&mut app, &mut doc);
            assert!(matches!(
                app.take_settled(invocation),
                Some(tilo_actions::InvocationUpdate::Failed { .. })
            ));
            assert_eq!(app.open_photo_id(), Some(id.as_str()));
            assert_eq!(crate::test_doc::library(&doc).photos.len(), 1);
        }

        eprintln!("phase content_restore_drops_active_viewer_drag_before_next_pointer_move");
        {
            let mut doc = AutoCommit::new();
            let store = ContentStore::memory();
            let bytes = pixels::encode_png(&crate::shots_pattern(6, 4)).unwrap();
            let open =
                OpenImage::from_path("same.png", 6, 4).with_content(store.put(&bytes).unwrap());
            let id = crate::test_doc::put_image(&mut doc, &open);
            crate::test_doc::set_photo_edits(&mut doc, &id, Edits::default().rotated_right());
            let selected = doc.get_heads();
            crate::test_doc::set_photo_edits(&mut doc, &id, Edits::default());
            let mut app = test_app_on(store);
            app.show_photo(&id);
            sync(&mut app, &mut doc);
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
            let device_id = winit::event::DeviceId::dummy();
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
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.,
                        modifiers: &winit::event::Modifiers::default(),
                        state: ui::widget::Edits::lend(&mut doc),
                        path: &ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
            }
            restore(&mut doc, &selected);
            present(&mut app, &doc);
            app.handle_event(
                &winit::event::WindowEvent::CursorMoved {
                    device_id,
                    position: winit::dpi::PhysicalPosition::new(center.x + 40., center.y + 20.),
                },
                &mut ui::widget::EventCtx {
                    scale_factor: 1.,
                    modifiers: &winit::event::Modifiers::default(),
                    state: ui::widget::Edits::lend(&mut doc),
                    path: &ROOT,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert_eq!(app.viewer.as_ref().unwrap().pan(), vello::kurbo::Vec2::ZERO);
            assert!((app.viewer.as_ref().unwrap().zoom() - 1.).abs() < f32::EPSILON);
            app.edit_shown(|photo| PhotoLibrary::ROTATE_RIGHT.with(photo));
            sync(&mut app, &mut doc);
            assert_eq!(crate::test_doc::photo_edits(&doc, &id).rotate_quarters, 2);
        }
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
        crate::test_doc::set_library(
            &mut doc,
            &crate::model::PhotoLibrary::of(photos, Vec::new()),
        );
        let mut app = test_app();
        app.library_mode = true;
        sync(&mut app, &mut doc);
        (app, doc, ids)
    }

    fn activate_album_control(app: &mut PhotosApp, doc: &mut AutoCommit, target: &str) {
        assert!(app.perform_semantic_action(
            target,
            &AccessAction::Activate,
            &mut ui::widget::Edits::lend(doc),
            &ROOT
        ));
        sync(app, doc);
    }

    fn seed_membership_album(doc: &mut AutoCommit, ids: &[String; 4]) {
        let mut library = crate::test_doc::library(doc);
        library.add_album(crate::model::Album {
            id: "photos.album:membership".into(),
            name: "Original".into(),
            photo_ids: crate::model::labels([ids[0].clone(), ids[3].clone()]),
        });
        crate::test_doc::set_library(doc, &library);
    }

    #[test]
    fn album_membership_edits_save_selected_ids_in_browse_order() {
        eprintln!("phase album_membership_create_saves_selected_ids_in_browse_order");
        {
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
            let library = crate::test_doc::library(&doc);
            assert_eq!(library.albums.len(), 1);
            assert_eq!(
                library.albums.values().next().unwrap().name,
                "Selected photographs"
            );
            let album = library.albums.values().next().unwrap();
            assert_eq!(
                library.album_photos(album).collect::<Vec<_>>(),
                [ids[0].as_str(), ids[3].as_str()],
                "an album shows its photographs in browse order"
            );
            assert!(!app.album_editor);
        }

        eprintln!("phase album_membership_rename_preserves_selected_ids_and_order");
        {
            let (mut app, mut doc, ids) = album_membership_fixture();
            seed_membership_album(&mut doc, &ids);
            sync(&mut app, &mut doc);
            activate_album_control(
                &mut app,
                &mut doc,
                "photos:library-album:photos.album:membership",
            );
            activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
            app.album_name_input.set_text("Renamed album");
            activate_album_control(&mut app, &mut doc, "photos:album-editor:save");
            let library = crate::test_doc::library(&doc);
            assert_eq!(library.albums.len(), 1);
            let album = library.album("photos.album:membership").unwrap();
            assert_eq!(album.name, "Renamed album");
            assert_eq!(
                library.album_photos(album).collect::<Vec<_>>(),
                [ids[0].as_str(), ids[3].as_str()]
            );
            assert!(!app.album_editor);
        }

        eprintln!("phase album_membership_after_restore_supports_normal_edit_and_browse_order");
        {
            let (mut app, mut doc, ids) = album_membership_fixture();
            seed_membership_album(&mut doc, &ids);
            let selected = doc.get_heads();
            let mut library = crate::test_doc::library(&doc);
            library.albums.iter_mut().next().unwrap().1.name = "Later album".into();
            library.albums.iter_mut().next().unwrap().1.photo_ids =
                crate::model::labels([ids[1].clone(), ids[2].clone()]);
            crate::test_doc::set_library(&mut doc, &library);
            sync(&mut app, &mut doc);
            activate_album_control(
                &mut app,
                &mut doc,
                "photos:library-album:photos.album:membership",
            );
            activate_album_control(&mut app, &mut doc, "photos:album-editor:edit");
            app.album_name_input.set_text("Unsaved later draft");
            restore(&mut doc, &selected);
            present(&mut app, &doc);
            sync(&mut app, &mut doc);
            assert!(!app.album_editor, "restore must detach the obsolete draft");
            let restored = crate::test_doc::library(&doc);
            let album = restored.album("photos.album:membership").unwrap();
            assert_eq!(album.name, "Original");
            assert_eq!(
                restored.album_photos(album).collect::<Vec<_>>(),
                [ids[0].as_str(), ids[3].as_str()]
            );

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
            let library = crate::test_doc::library(&doc);
            let album = library.album("photos.album:membership").unwrap();
            assert_eq!(album.name, "Edited after restore");
            assert_eq!(
                library.album_photos(album).collect::<Vec<_>>(),
                [ids[0].as_str(), ids[2].as_str()]
            );
            assert_eq!(library.photos.len(), 4);
            assert!(!app.album_editor);
        }
    }
}
