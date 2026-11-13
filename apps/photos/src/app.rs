//! The root widget: a toolbar, the image canvas, and a status line. The buttons are
//! [`ui::widget::Button`], the canvas is [`ui::widget::ImageViewer`], and the nothing-open state is
//! [`ui::widget::EmptyState`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, SyncSender};

use tilo_content::ContentStore;

use crate::actions::PhotosEffect;
use vello::kurbo::{Point, Rect};
use vello::peniko::ImageData;

use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{
    Button, DateIndexMarker, DateIndexedScrollbar, ImageViewer, ModalLayout, ProgressBar,
    ScrollMetrics, ScrollRegion, TextInput, TextInputKind, Widget,
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
mod window;

pub use window::PhotosApp;
pub(crate) use window::view;

use directory_import::build_directory_import_buttons;
use editors::{build_metadata_editor_buttons, clear_toggled_input};
use jobs::with_thumbnail_wake;
use navigation::build_collection_page_buttons;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CardActions {
    details: Rect,
    remove: Rect,
    favorite: Rect,
}

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
/// One bounded materialized library page. This is intentionally independent of the number of cards
/// that happen to fit in a particular window.
const LIBRARY_PAGE_SIZE: usize = 24;

/// How many seconds a photograph is shown before the slide show moves on: enough to look at it, and
/// short enough that a workspace does not wait. It is the content's own pace, not a motion's, so it
/// is a number of seconds and not a duration constant: a slide show dwell is a preference waiting
/// to be a setting, and a duration here reads as animation timing to anyone scanning.
const SLIDE_SHOW_SECONDS: u64 = 5;

/// The levels the open photograph zooms through, from smaller than its frame
/// to eight times it, with its own size (fitted to the frame) where `Ctrl+0`
/// returns. A photograph's scale is the picture's, never the window's text.
const PHOTO_ZOOM_LEVELS: [f32; 10] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0];
/// People, places, and albums are projected one bounded page at a time. The backing vectors retain
/// the full collection so “Browse all” is real rather than a prettier name for truncation.
const COLLECTION_PAGE_SIZE: usize = 4;

/// Target heights of a timeline row at each density step, in lines of body
/// text so they grow with the text scale: spacious (0) to compact (4).
const TIMELINE_ROW_LINES: [f64; 5] = [15.0, 11.5, 9.0, 7.0, 5.5];

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

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct PeopleScanProgress {
    next_offset: usize,
    completed_batches: u64,
    last_library_size: usize,
    status: String,
    last_error: Option<String>,
}

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
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Present,
    SlideShow,
    Selection(SelectionHit),
    PeopleHidden,
    PreviousPage,
    NextPage,
}

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
    Place(String),
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
    width: f64,
}

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
    favorites: bool,
    bounds: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CanvasState {
    #[default]
    Empty,
    Loading,
    Error,
    Ready,
}

type DecodedPixels = Arc<std::sync::Mutex<Vec<(String, Option<Rgba>)>>>;

#[derive(Clone, PartialEq)]
struct LiveContent {
    library: crate::model::PhotoLibrary,
}

struct Rendered {
    identity: String,
    edits: Edits,
    rgba: Rgba,
}

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
pub struct Photos {
    dispatch: Option<ui::framework::Dispatcher<crate::model::PhotoLibrary>>,
    opens: Rc<RefCell<Vec<Id>>>,
    actions: Option<ui::framework::Actions<crate::model::PhotoLibrary>>,
    app_name: String,
    content: ContentStore,
    #[cfg(test)]
    last_library_action: Option<ui::tilo_resource::Invoke<PhotoLibrary>>,
    original: Option<(String, Rgba)>,
    original_identity: Option<String>,
    decoding: Option<String>,
    decoded: DecodedPixels,
    observed_content: Option<LiveContent>,
    import_content: Option<LiveContent>,
    picker_stale: bool,
    rendered: Option<Rendered>,
    viewer: Option<ImageViewer>,
    viewer_details: Option<ViewerDetails>,
    chrome: chrome::Chrome,
    layout_theme: Theme,
    pending: Rc<Cell<Option<Hit>>>,
    notice: Option<String>,
    state_save_failure: Option<String>,
    notice_said: ui::widget::toast::Transient<String>,
    notice_last: Option<String>,
    pub(crate) copy: ui::CopyWatch,
    rect: Rect,
    canvas_rect: Rect,
    status_rect: Rect,
    presenting: bool,
    slide_show: Option<ui::time::Instant>,
    viewer_zoom: ui::content_zoom::ZoomModel,
    picker: Option<ui::portal::PortalChannel>,
    contents_picker: Option<ui::portal::ContentsChannel>,
    directory_picker: Option<ui::portal::PortalChannel>,
    save_chooser: Option<(ui::portal::SaveChannel, Option<tilo_actions::InvocationId>)>,
    directory_picker_retain_exif_location: bool,
    directory_import: Option<DirectoryImport>,
    directory_import_receipt: Option<DirectoryImportReceipt>,
    directory_import_details_open: bool,
    directory_import_details_scroll: ui::widget::ScrollRegion,
    directory_import_details_layout: Option<ModalLayout>,
    directory_import_pending: Rc<Cell<Option<DirectoryStatusHit>>>,
    directory_import_buttons: Vec<(DirectoryStatusHit, Button)>,
    directory_import_progress: ProgressBar,
    resource_invocation: Option<tilo_actions::InvocationId>,
    replies: HashMap<tilo_actions::InvocationId, ui::framework::EffectReply>,
    #[cfg(test)]
    settled: Vec<tilo_actions::InvocationUpdate>,
    pending_resource_open: Option<(tilo_actions::InvocationId, String)>,
    analysis_tx: Option<SyncSender<AnalysisJob>>,
    analysis_result_tx: ui::wake::WakingSyncSender<AnalysisResult>,
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
    library_analysis_coverage: (usize, usize, usize),
    people_controller: crate::people::PeopleController,
    people_scan_progress: PeopleScanProgress,
    people_mode: bool,
    people_selection: Option<PeopleSelection>,
    people_detail_transition: ui::motion::Presence,
    people_merge_mode: bool,
    people_show_hidden: bool,
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
    search_revision: Option<u64>,
    search_refresh_error: Option<String>,
    #[cfg(test)]
    force_search_refresh_error: bool,
    library_items: Vec<LibraryItem>,
    library_total: usize,
    library_photo_count: usize,
    structure_dirty: bool,
    library_destination: LibraryDestination,
    places_list_mode: bool,
    places_viewport: crate::places::MapViewport,
    places_fit_pending: bool,
    places_open_key: Option<String>,
    places_items: Vec<PlaceItem>,
    places_page: usize,
    places_hits: Vec<(PlacesHit, Rect)>,
    collection_page_hits: Vec<(CollectionPageHit, Rect, bool)>,
    collection_page_buttons: Vec<(CollectionPageHit, Button)>,
    collection_page_pending: Rc<Cell<Option<CollectionPageHit>>>,
    places_map_bounds: Rect,
    places_drag_origin: Option<Point>,
    places_camera_feedback: Option<ui::motion::Feedback>,
    places_motion_reduced: bool,
    library_favorite_count: usize,
    library_offset: usize,
    library_albums: Vec<(String, String, usize)>,
    library_album_covers: HashMap<String, Vec<String>>,
    albums_scroll: f64,
    albums_page: usize,
    albums_viewport: Rect,
    selection_album_page: usize,
    active_album: Option<String>,
    active_folder: Option<String>,
    active_person: Option<String>,
    active_tag: Option<String>,
    active_object: Option<String>,
    active_pet: Option<String>,
    active_favorites: bool,
    library_folders: Vec<(String, usize)>,
    library_people: Vec<(String, usize)>,
    people_page: usize,
    library_person_covers: HashMap<String, String>,
    library_tags: Vec<(String, usize)>,
    metadata_filter_cards: Vec<MetadataFilterCard>,
    library_page_hits: Vec<(bool, bool, Rect)>,
    timeline_density: u8,
    timeline_scroll_y: f64,
    timeline_projection: TimelineProjection,
    timeline_projection_revision: Option<u64>,
    timeline_projection_filter: String,
    timeline_viewport_height: f64,
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
    album_editor_focus: usize,
    album_editor_rect: Rect,
    album_editor_hits: Vec<(AlbumEditorHit, Rect)>,
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
    metadata_location_from_map: bool,
    metadata_mixed: [bool; 5],
    metadata_force_clear: [bool; 5],
    metadata_seed: [String; 7],
    metadata_editor_section: MetadataEditorSection,
    metadata_editor_focus: usize,
    metadata_editor_hits: Vec<(MetadataEditorHit, Rect)>,
    editor_scroll: ScrollRegion,
    editor_field_bands: Vec<Rect>,
    editor_scroll_viewport: Rect,
    metadata_editor_reveal_focus: bool,
    metadata_editor_buttons: Vec<(MetadataEditorHit, Button)>,
    metadata_editor_pending: Rc<Cell<Option<MetadataEditorHit>>>,
    library_thumbnails: HashMap<String, ImageData>,
    thumbnail_resource_keys: HashMap<String, String>,
    thumbnail_unavailable: HashSet<String>,
    original_decode_failed: Option<String>,
    thumbnail_service: thumbnail::ThumbnailService,
    thumbnail_keys: HashMap<String, thumbnail::ThumbKey>,
    thumbnail_generation: Option<u64>,
    thumbnail_visible_ids: Vec<String>,
    thumbnail_queued: HashSet<String>,
    library_remove_confirm: Option<String>,
    selection_mode: bool,
    selected_photo_ids: HashSet<String>,
    selection_anchor: Option<String>,
    selection_destination_album: Option<String>,
    selection_album_chooser: bool,
    selection_album_scroll: ui::widget::ScrollRegion,
    selection_all_favorite: bool,
    selection_hits: Vec<(SelectionHit, Rect)>,
    selection_destination_hits: Vec<(String, Rect)>,
    library_cards: Vec<(String, Rect, bool)>,
    pointer: Point,
    library_mode: bool,
    presented: Rc<crate::model::PhotoLibrary>,
    presented_revision: u64,
    open_photo: Option<String>,
    launched: Option<std::path::PathBuf>,
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

impl Photos {
    #[allow(
        clippy::too_many_lines,
        reason = "construction lists every app-owned state field explicitly so local, shared, and transient state remain auditable"
    )]
    pub fn with_content(content: ContentStore) -> Self {
        let (analysis_result_tx, analysis_rx) = ui::wake::waking_sync_channel(ANALYSIS_QUEUE_LIMIT);
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
            decoding: None,
            decoded: Arc::default(),
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
            state_save_failure: None,
            notice_said: ui::widget::toast::Transient::new(),
            notice_last: None,
            copy: ui::CopyWatch::default(),
            rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            presenting: false,
            slide_show: None,
            viewer_zoom: ui::content_zoom::ZoomModel::stepping(PHOTO_ZOOM_LEVELS, 1.0),
            picker: None,
            contents_picker: None,
            directory_picker: None,
            save_chooser: None,
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
            metadata_seed: Default::default(),
            metadata_editor_section: MetadataEditorSection::Organize,
            metadata_editor_focus: 0,
            metadata_editor_hits: Vec::new(),
            editor_scroll: ScrollRegion::vertical().with_edge_fade(),
            editor_field_bands: Vec::new(),
            editor_scroll_viewport: Rect::ZERO,
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
            library_mode: true,
            presented: Rc::default(),
            presented_revision: 0,
            open_photo: None,
            launched: None,
            staged: None,
        }
    }

    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        self.staged = Some(scenario.to_owned());
    }

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
                ui::tilo_resource::Bytes::kept_as(&bytes, &tilo_content::hash_bytes(&bytes)),
            );
            self.show_photo(&LibraryPhoto::from_image(image).id);
        }
    }

    fn apply_staged(&mut self) {
        let Some(scenario) = self.staged.take() else {
            return;
        };
        if let Some(bar) = &self.chrome.bar {
            bar.stage_pane(ui::widget::NavPane::Content);
        }
        #[cfg(feature = "samples")]
        {
            self.open_photo = crate::sample::opened(&scenario);
        }
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
            #[cfg(feature = "samples")]
            "people" | "people-detail" => {
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
            #[cfg(feature = "samples")]
            scenario if crate::sample::shows_library(scenario) => self.show_library(),

            _ => {}
        }
    }

    pub fn open_path(&mut self, path: &str) {
        self.open_bytes(path, bounded_file_bytes(path).as_deref());
    }

    pub fn open_contents(&mut self, name: &str, bytes: &[u8]) {
        let bytes = (bytes.len() <= pixels::MAX_ENCODED_BYTES).then_some(bytes);
        self.open_bytes(name, bytes);
    }

    fn open_bytes(&mut self, path: &str, bytes: Option<&[u8]>) {
        let Some((width, height)) = bytes.and_then(pixels::size) else {
            self.notice = Some(tr("photos-open-failed"));
            return;
        };
        let bytes = bytes.unwrap_or_default();
        let content = match self.content.put(bytes) {
            Ok(reference) => ui::tilo_resource::Bytes::kept_as(bytes, &reference),
            Err(error) => {
                tracing::warn!(%error, "photos: could not store an opened original");
                self.notice = Some(tr("photos-open-failed"));
                return;
            }
        };
        let open = OpenImage::new(path, width, height, content);
        self.original = None;
        self.original_identity = None;
        self.decoding = None;
        self.rendered = None;
        self.close_viewer();
        self.original_decode_failed = None;
        let photo = LibraryPhoto::from_image(open);
        let photo_id = photo.id.clone();
        self.commit(crate::resource::import(&photo));
        self.show_photo(&photo_id);
        self.notice = None;
    }

    pub fn open_image_path(&self) -> Option<&str> {
        self.original.as_ref().map(|(p, _)| p.as_str())
    }

    pub fn edited_pixels(&self) -> Option<&Rgba> {
        self.rendered.as_ref().map(|r| &r.rgba)
    }

    pub fn canvas_rect(&self) -> Rect {
        self.canvas_rect
    }

    pub fn status_rect(&self) -> Rect {
        self.status_rect
    }

    pub fn window_bar_names(&self) -> Vec<String> {
        let toolbar = self.window_bar();
        toolbar
            .leading
            .iter()
            .chain(&toolbar.actions)
            .map(|action| action.name.clone())
            .collect()
    }

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

    pub fn status(&self) -> String {
        [&self.chrome.status_left, &self.chrome.status_right]
            .into_iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn shows_library(&self) -> bool {
        self.library_mode
    }

    pub fn has_image(&self) -> bool {
        self.viewer.is_some()
    }

    #[must_use]
    pub fn new_persistent(content: ContentStore) -> Self {
        let mut app = Self::with_content(content);
        app.people_controller = crate::people::PeopleController::persistent();
        if let Ok(service) = thumbnail::ThumbnailService::open_default(1) {
            app.thumbnail_service = with_thumbnail_wake(service);
        }
        app
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.set_zoom(zoom);
        }
    }

    pub fn visible_rect(&self) -> Rect01 {
        match (self.viewer.as_ref(), self.rendered.as_ref()) {
            (Some(v), Some(rendered)) => {
                visible_image_rect(v, f64::from(rendered.rgba.w), f64::from(rendered.rgba.h))
                    .unwrap_or(Rect01::FULL)
            }
            _ => Rect01::FULL,
        }
    }

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

    pub fn save_to(&mut self, path: &ui::tilo_resource::AbsolutePath) -> bool {
        let saved = self.edited_png().is_some_and(|bytes| {
            tilo_files::file_at(path)
                .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes))
                .is_ok()
        });
        self.notice = Some(if saved {
            format!("{} {path}", tr("photos-saved"))
        } else {
            tr("photos-save-failed")
        });
        saved
    }

    pub fn save_with_chooser(&mut self) -> bool {
        self.shown_photo()
            .is_some_and(|photo| self.choose_save_destination(&photo.image, None))
    }

    pub fn is_saving(&self) -> bool {
        self.save_chooser.is_some()
    }

    pub(crate) fn choose_save_destination(
        &mut self,
        open: &OpenImage,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> bool {
        if self.save_chooser.is_some() {
            return false;
        }
        let Some(bytes) = self.edited_png() else {
            self.notice = Some(tr("photos-save-failed"));
            return false;
        };
        let filter = ui::portal::FileFilter::new(&tr("photos-filter-images")).mimetype("image/png");
        let chooser = ui::portal::PickRequest::save(
            &tr("photos-save-title"),
            &tr("photos-save"),
            &Self::suggested_save_name(open),
        )
        .thread_name("photos-portal-save")
        .filter(filter)
        .send_save_contents(bytes);
        self.save_chooser = Some((chooser, invocation));
        true
    }

    fn edited_png(&self) -> Option<Vec<u8>> {
        self.edited_pixels().and_then(pixels::encode_png)
    }

    pub fn suggested_save_name(open: &OpenImage) -> String {
        let stem = std::path::Path::new(&open.name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "photo".to_owned());
        format!("{stem}-edited.png")
    }

    fn edit_shown<S: ui::framework::Submit<PhotoLibrary>>(&mut self, edit: impl FnOnce(Id) -> S) {
        let Some(photo_id) = self.open_photo.clone() else {
            return;
        };
        if let Err(error) = self.submit(edit(Id::from_raw(photo_id))) {
            tracing::warn!(%error, "photos: the edit was refused");
        }
        self.notice = None;
    }

    pub(crate) fn shown_photo(&self) -> Option<LibraryPhoto> {
        self.shown_photo_in(&self.presented())
    }

    fn presented(&self) -> Rc<crate::model::PhotoLibrary> {
        Rc::clone(&self.presented)
    }

    fn shown_photo_in(&self, library: &crate::model::PhotoLibrary) -> Option<LibraryPhoto> {
        library.photo(self.open_photo.as_deref()?).cloned()
    }

    pub fn open_photo_id(&self) -> Option<&str> {
        self.open_photo.as_deref()
    }

    #[doc(hidden)]
    pub fn adopt_resource_state(&mut self, state: &Rc<crate::model::PhotoLibrary>) {
        if *self.presented == **state {
            return;
        }
        self.presented = Rc::clone(state);
        self.presented_revision = self.presented_revision.wrapping_add(1);
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::model::PhotoLibrary>,
        actions: ui::framework::Actions<crate::model::PhotoLibrary>,
    ) {
        self.dispatch = Some(dispatcher);
        self.actions = Some(actions);
        self.bind_controls();
    }

    fn share_open_photo(&mut self) {
        match (&self.open_photo, &self.dispatch) {
            (Some(photo), Some(dispatch)) => {
                dispatch.share_link(&crate::resource::photo_resource_id(photo.as_str()));
            }
            _ => self.notice = Some(tr("photos-share-failed")),
        }
    }

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

    fn say_when_kept(&mut self, text: String) {
        match &self.dispatch {
            Some(dispatch) => dispatch.say_when_kept(text),
            None => self.notice = Some(text),
        }
    }

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

    fn commit_group(&mut self, edits: &[ui::tilo_resource::Edit<PhotoLibrary>]) {
        let members: Vec<_> = edits
            .iter()
            .filter_map(|edit| ui::tilo_resource::Invoke::edit(edit).ok())
            .collect();
        if !members.is_empty() {
            self.commit(ui::tilo_resource::Invoke::group(members));
        }
    }

    fn viewer_details_for(
        library: &crate::model::PhotoLibrary,
        open: &OpenImage,
        edits: Edits,
    ) -> ViewerDetails {
        let photo = library
            .photos
            .values()
            .find(|photo| photo.image.hash() == open.hash())
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
            for (decoded, pixels) in self.take_decoded() {
                if decoded != key {
                    continue;
                }
                self.decoding = None;
                if let Some(img) = pixels {
                    self.original = Some((open.name.clone(), img));
                    self.original_identity = Some(identity.clone());
                    self.original_decode_failed = None;
                } else {
                    self.original = None;
                    self.rendered = None;
                    self.close_viewer();
                    self.original_decode_failed = Some(key);
                    self.canvas_state = CanvasState::Error;
                    return;
                }
            }
            if self.original_identity.as_ref() != Some(&identity) {
                self.original = None;
                self.rendered = None;
                self.close_viewer();
                self.canvas_state = CanvasState::Loading;
                if self.decoding.as_deref() != Some(key.as_str()) {
                    match crate::model::bytes_of(&open, &self.content) {
                        Some(bytes) => self.start_decode(key, bytes),
                        None => self.decoding = None,
                    }
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
        self.viewer = Some(ImageViewer::new(image));
        self.structure_dirty = true;
        self.canvas_state = CanvasState::Ready;
        self.rendered = Some(Rendered {
            identity,
            edits,
            rgba,
        });
    }

    fn start_decode(&mut self, key: String, bytes: Vec<u8>) {
        let decoded = Arc::clone(&self.decoded);
        let wake = ui::WakeHandle::new();
        let landed = key.clone();
        ui::rt::app().spawn_then(
            async move { pixels::decode_async(&bytes).await },
            move |pixels| {
                decoded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((landed, pixels));
                wake.wake();
            },
        );
        self.decoding = Some(key);
    }

    fn take_decoded(&self) -> Vec<(String, Option<Rgba>)> {
        std::mem::take(
            &mut *self
                .decoded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
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

    fn library_empty_state_text(&self) -> (String, String) {
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

    fn empty_shown(&self) -> Option<EmptyShown> {
        let (title, body, icon, action) = if self.library_mode {
            match self.library_destination {
                _ if self.people_mode => return None,
                LibraryDestination::Albums if self.library_albums.is_empty() => {
                    let new_album = (!self.album_editor).then_some(EmptyAction::NewAlbum);
                    (
                        tr("photos-albums-empty-title"),
                        tr("photos-albums-empty-body"),
                        Icon::GalleryVerticalEnd,
                        new_album,
                    )
                }
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
}

#[derive(Clone, Debug, PartialEq)]
struct EmptyShown {
    title: String,
    body: String,
    icon: Icon,
    error: bool,
    action: Option<EmptyAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyAction {
    Open,
    NewAlbum,
}

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

pub(crate) fn dimensions_label(width: u32, height: u32) -> String {
    format!("{width} × {height}")
}

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

fn to_peniko(img: &Rgba) -> ImageData {
    pixels::to_peniko(img)
}

fn decode_key(open: &OpenImage) -> String {
    open.hash()
}

fn thumbnail_key(open: &OpenImage) -> String {
    format!("{}:480x480", decode_key(open))
}

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

fn capture_day(captured_at: &str) -> String {
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

fn bounded_file_bytes(path: &str) -> Option<Vec<u8>> {
    crate::files::read_native(std::path::Path::new(path), pixels::MAX_ENCODED_BYTES as u64)
}

impl Photos {
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

impl Photos {
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
