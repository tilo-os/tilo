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
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender};

use automerge::AutoCommit;
use tilo_content::ContentStore;

use crate::actions::PhotosEffect;
use vello::kurbo::{Point, Rect};
use vello::peniko::ImageData;

use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{
    Button, DateIndexMarker, DateIndexedScrollbar, EmptyState, ImageViewer, ModalLayout,
    ProgressBar, ScrollMetrics, TextInput, TextInputKind, Widget,
};

use crate::model::{Edits, LibraryPhoto, OpenImage, PhotoLibrary, Rect01};
use crate::pixels::{self, Rgba};
use crate::strings::{tr, tr_args};
use crate::timeline_projection::TimelineProjection;
use ui::ArgValue;
use ui::tilo_resource::Id;

mod accessibility;
mod chrome;
mod directory_import;
mod editors;
mod effects;
mod jobs;
mod navigation;
mod paint;
mod people;
mod widget;

use directory_import::build_directory_import_buttons;
use editors::{build_metadata_editor_buttons, clear_toggled_input};
use jobs::with_thumbnail_wake;
use navigation::build_collection_page_buttons;

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
    content: String,
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
    /// Where the last export went, or why it failed. Said in the window's
    /// bar so an action is never silent.
    notice: Option<String>,
    /// [`Self::notice`] in the window's bar for a while once it changes.
    notice_said: ui::widget::toast::Transient<String>,
    /// The notice last said, so only a change is news.
    notice_last: Option<String>,
    /// The last copy, whose outcome can arrive after its notice.
    pub(crate) copy: ui::CopyWatch,
    /// What the content column shows while it has nothing to lay out.
    empty_state: Option<EmptyState>,
    /// What the empty state shows, and whether the framework's door was
    /// attached to bind its action.
    empty_state_key: Option<(EmptyShown, bool)>,
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
    people_detail_transition: ui::motion::Presence,
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
    places_camera_feedback: Option<ui::motion::Feedback>,
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
    /// hashes are immutable, so retaining this failure is safe.
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
        let chrome = chrome::Chrome::new();

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
            notice_said: ui::widget::toast::Transient::new(),
            notice_last: None,
            copy: ui::CopyWatch::default(),
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
            people_detail_transition: ui::motion::Presence::closed(),
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
            // A window opens on the Timeline; showing a photograph leaves it.
            library_mode: true,
            presented: Rc::default(),
            presented_revision: 0,
            open_photo: None,
            launched: None,
            staged: None,
        }
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
            let image = OpenImage::new(
                &path.to_string_lossy(),
                1,
                1,
                tilo_content::hash_bytes(&bytes),
            );
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
        // A scenario that names a photograph shows it; the library views
        // below switch back to the library themselves.
        if self.open_photo.is_some() {
            self.library_mode = false;
        }
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
                let open = OpenImage::new(path, img.w, img.h, content);
                self.original = Some((path.to_string(), img));
                self.original_identity = Some(decode_key(&open));
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

    /// The status bar's rect; zero height when it has nothing to show.
    pub fn status_rect(&self) -> Rect {
        self.status_rect
    }

    /// The names of what the window bar offers for what is shown, in
    /// order: Back while a photograph or a person is open, then the
    /// commands, those the bar's row has no room for in its menu.
    pub fn window_bar_names(&self) -> Vec<String> {
        let toolbar = self.window_bar();
        toolbar
            .leading
            .iter()
            .chain(&toolbar.actions)
            .map(|action| action.name.clone())
            .collect()
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

    /// Zoom the canvas, as the scroll wheel turned at its centre would: the
    /// pan stays. Lets a caller frame a crop.
    pub fn set_zoom(&mut self, zoom: f32) {
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.set_zoom(zoom);
        }
    }

    /// The visible region of the canvas in normalized image coordinates, or
    /// the whole image when the viewer has not been laid out yet.
    ///
    /// `ImageViewer` exposes no direct query for this either, so it is derived
    /// in `visible_image_rect` from the same "contain fit, then zoom and
    /// pan" transform `ImageViewer::draw` applies — using only the widget's
    /// already-public `zoom()` / `pan()` / rect, and the rendered image's
    /// pixel size, after the current crop and rotation.
    pub fn visible_rect(&self) -> Rect01 {
        match (self.viewer.as_ref(), self.rendered.as_ref()) {
            (Some(v), Some(rendered)) => {
                visible_image_rect(v, f64::from(rendered.rgba.w), f64::from(rendered.rgba.h))
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
        let ok = self.copy.watch(ui::clipboard_copy_image(
            img.w as usize,
            img.h as usize,
            &img.bytes,
        ));
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
        let p = std::path::Path::new(&open.name);
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

    /// Ask for the open photograph's link: the framework mints it from the
    /// photograph's view and copies it, as for an agent asking for
    /// `share-link`, so every path shares the same URL.
    fn share_open_photo(&mut self) {
        match (&self.open_photo, &self.dispatch) {
            (Some(photo), Some(dispatch)) => {
                dispatch.share_link(&crate::resource::photo_resource_id(photo.as_str()));
            }
            _ => self.notice = Some(tr("photos-share-failed")),
        }
    }

    /// Say what asking for a photograph's link came to, once.
    fn drain_share_outcome(&mut self) {
        let Some(dispatch) = &self.dispatch else {
            return;
        };
        let share = ui::tilo_resource::share::SHARE_LINK;
        let landed = dispatch.landed();
        if landed.with(|landed| landed.as_ref().is_some_and(|landed| landed.action == share)) {
            landed.set(None);
            self.notice = Some(tr("photos-share-copied"));
        }
        let refused = dispatch.refused();
        let reason = refused.with(|refused| {
            refused
                .as_ref()
                .filter(|refused| refused.action == share)
                .map(|refused| refused.reason.clone())
        });
        // What this window said it did, once the room kept it
        // ([`Self::say_when_kept`]); a refusal, said after it, wins.
        let said = dispatch.said();
        if let Some(text) = said.with(Clone::clone) {
            said.set(None);
            self.notice = Some(text);
        }
        if let Some(reason) = reason {
            refused.set(None);
            self.notice = Some(reason);
        }
    }

    /// Say `text` of a library change just submitted once it is kept: at
    /// once in a library nobody else decides, once the room accepts it in a
    /// shared one, and never when the room refuses it.
    fn say_when_kept(&mut self, text: String) {
        match &self.dispatch {
            Some(dispatch) => dispatch.say_when_kept(text),
            None => self.notice = Some(text),
        }
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

    /// Project the open photograph's portable library record into the small
    /// viewer card, found by its content identity.
    fn viewer_details_for(
        library: &crate::model::PhotoLibrary,
        open: &OpenImage,
        edits: Edits,
    ) -> ViewerDetails {
        let photo = library
            .photos
            .values()
            .find(|photo| photo.image.content == open.content)
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
                self.original = Some((open.name, img));
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
    fn library_empty_state_text(&self) -> (String, String) {
        // An empty collection is not an empty library: Favorites or a new
        // album say what fills them, and offer no "Open a photo" they lack.
        let (title, body) = if self.active_favorites {
            (
                "photos-favorites-empty-title",
                "photos-favorites-empty-body",
            )
        } else if self.active_album.is_some() {
            ("photos-album-empty-title", "photos-album-empty-body")
        } else if !self.timeline_unfiltered() {
            ("photos-filter-empty-title", "photos-filter-empty-body")
        } else {
            ("photos-library-empty-title", "photos-library-empty-body")
        };
        (tr(title), tr(body))
    }

    /// What the content column shows when it has nothing to lay out, if
    /// that is the case: the open photograph's absence, an empty timeline,
    /// no albums or no places.
    fn empty_shown(&self) -> Option<EmptyShown> {
        let (title, body, icon, action) = if self.library_mode {
            match self.library_destination {
                _ if self.people_mode => return None,
                LibraryDestination::Albums if self.library_albums.is_empty() => (
                    tr("photos-albums-empty-title"),
                    tr("photos-albums-empty-body"),
                    Icon::GalleryVerticalEnd,
                    Some(EmptyAction::NewAlbum),
                ),
                LibraryDestination::Places if self.places_items.is_empty() => (
                    tr("photos-places-empty-title"),
                    tr("photos-places-empty-body"),
                    Icon::Globe,
                    None,
                ),
                LibraryDestination::Timeline if self.library_items.is_empty() => {
                    let (title, body) = self.library_empty_state_text();
                    let open = self.content_offers_open().then_some(EmptyAction::Open);
                    (title, body, Icon::Image, open)
                }
                _ => return None,
            }
        } else if self.viewer.is_none() {
            let (title, body) = self.empty_state_text();
            let open = self.content_offers_open().then_some(EmptyAction::Open);
            (title, body, Icon::Image, open)
        } else {
            return None;
        };
        Some(EmptyShown {
            title,
            body,
            icon,
            error: !self.library_mode && self.canvas_state == CanvasState::Error,
            action,
        })
    }

    /// Keep the shared empty state mounted as a retained child whenever the
    /// content column is empty, so keyboard, accessibility and automation
    /// reach its action through the same door as a click.
    fn refresh_empty_state(&mut self, doc: &AutoCommit) {
        let key = self
            .empty_shown()
            .map(|shown| (shown, self.actions.is_some()));
        if key != self.empty_state_key {
            self.empty_state = key.as_ref().map(|(shown, _)| {
                let empty = if shown.error {
                    EmptyState::error(&shown.title, &shown.body)
                } else {
                    EmptyState::new(&shown.title).with_message(&shown.body)
                }
                .with_icon(shown.icon);
                // Import and a new album are effects of the app, not actions
                // of the library record: they bind through the framework's
                // door, as the header's Open does.
                let (label, id, effect) = match shown.action {
                    None => return empty,
                    Some(EmptyAction::Open) => (
                        tr("photos-open"),
                        "photos-empty-open",
                        PhotosEffect::Import { path: None },
                    ),
                    Some(EmptyAction::NewAlbum) => (
                        tr("photos-new-album"),
                        "photos-empty-new-album",
                        PhotosEffect::NewAlbum,
                    ),
                };
                let empty = empty.with_action(label).with_action_id(id);
                match self
                    .actions
                    .as_ref()
                    .and_then(|actions| actions.effect(&effect))
                {
                    Some(binding) => empty.on_action(binding),
                    None => empty,
                }
            });
            self.empty_state_key = key;
            self.structure_dirty = true;
        }
        if let Some(empty) = &mut self.empty_state {
            empty.sync(doc, &automerge::ROOT);
        }
    }
}

/// What the empty content column says and offers.
#[derive(Clone, Debug, PartialEq)]
struct EmptyShown {
    title: String,
    body: String,
    icon: Icon,
    error: bool,
    action: Option<EmptyAction>,
}

/// The one way forward an empty content column offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyAction {
    Open,
    NewAlbum,
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

/// A capture time as people read it: the toolkit's timestamp style
/// (`2026-07-12 18:16`, in the person's formats) when the library stored an
/// ISO time, else the date and the time it holds.
fn captured_label(captured_at: &str) -> String {
    if let Ok(at) = ui::format::NaiveDateTime::parse_from_str(captured_at, "%Y-%m-%dT%H:%M") {
        return ui::format::timestamp(&at);
    }
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

/// A local decode-cache key: the content hash, which is immutable.
fn decode_key(open: &OpenImage) -> String {
    open.content.clone()
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
    let date = known_month
        .then(|| {
            ui::format::NaiveDate::from_ymd_opt(
                year.parse().ok()?,
                month.parse().ok()?,
                day_of_month.parse().ok()?,
            )
        })
        .flatten();
    date.map_or(day, ui::format::month_day_year)
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
fn bounded_file_bytes(path: &str) -> Option<Vec<u8>> {
    crate::files::read_native(std::path::Path::new(path), pixels::MAX_ENCODED_BYTES as u64)
}

impl PhotosApp {
    fn retained_buttons_mut(&mut self) -> Vec<&mut Button> {
        let mut buttons: Vec<&mut Button> = Vec::new();
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

#[cfg(test)]
#[path = "app/tests.rs"]
mod tests;

#[cfg(test)]
mod history_reconcile_tests;
