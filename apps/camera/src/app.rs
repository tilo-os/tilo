//! Root widget for the Camera app.
//!
//! A canvas-heavy app: rather than composing dozens of taffy nodes, the
//! root occupies a single layout node and paints every screen (Camera,
//! Scanner, and Settings) straight into the Vello scene, hit-testing
//! its own controls.
//!
//! A shutter press writes verified media bytes and retains one contextual
//! recent-capture handoff. Photos and Video Player own collection browsing,
//! favorites, deletion, and metadata. The Automerge document (see
//! [`crate::doc`]) holds the recent handoff, scan history, and granular capture
//! preferences. A
//! live viewfinder needs a camera; when none is
//! present the camera/scanner screens say so honestly rather than faking a
//! feed. The hermetic `--shots` / test path uses a self-contained sample
//! source so screenshots and tests never touch the disk or a device.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
#[cfg(test)]
use tilo_actions::EncodedAction;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::ActionEmitter;
use ui::widget::{CropSelection, Image, ImageCropEditor, ImageFit, Popover, Widget};
use winit::event::WindowEvent;

use crate::doc;
use crate::frame::{self, FrameBuf};
use crate::library;
use crate::model::{
    Capture, CaptureKind, CaptureMode, CodeKind, Facing, PhotoFormat, Resolution, ScanEntry,
    ScannedCode, Screen, Settings, Timer,
};

mod document_view;
mod hud;
mod scanner_view;
mod settings_sheet;

/// Payload composited into the sample scanner poster (hermetic path only).
const SCAN_PAYLOAD: &str = "WIFI:T:WPA2;S:Tilo Lounge;P:welcome-aboard;H:false;;";
/// Single decoded-size budget for the contextual recent-capture preview.
const THUMB: (u32, u32) = (512, 342);
/// Renderer/upload admission limit for external camera data. This mirrors the
/// capture callback limit and remains here for the feature-off/test path: no
/// caller can smuggle malformed pixels into an `Image`.
const MAX_FRAME_PIXELS: u64 = 8_847_360; // 4096 × 2160
const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;
/// Keep only the current and immediately superseded thumbnail while an async
/// recent-capture handoff changes; Camera never owns a browsing cache.
const MAX_THUMB_CACHE: usize = 2;

/// A deterministic paper-like fixture for screenshots and interaction tests.
/// It deliberately contains varied blocks, rules and columns so crop,
/// rotation, scaling and page previews exercise real contrast and geometry.
fn sample_document_image() -> image::DynamicImage {
    let page = image::RgbImage::from_fn(1240, 1754, |x, y| {
        let in_rect = |x0, y0, x1, y1| x >= x0 && x < x1 && y >= y0 && y < y1;
        if in_rect(80, 80, 1160, 230) {
            if in_rect(130, 125, 600, 158) || in_rect(130, 174, 430, 193) {
                image::Rgb([245, 248, 255])
            } else {
                image::Rgb([52, 99, 176])
            }
        } else if in_rect(80, 290, 500, 312)
            || in_rect(80, 338, 740, 351)
            || in_rect(80, 374, 650, 387)
            || in_rect(80, 410, 790, 423)
            || in_rect(80, 510, 330, 532)
            || in_rect(80, 562, 1120, 575)
            || in_rect(80, 600, 1020, 613)
            || in_rect(80, 638, 1080, 651)
            || in_rect(80, 790, 390, 812)
            || in_rect(80, 842, 1110, 855)
            || in_rect(80, 880, 950, 893)
            || in_rect(80, 918, 1040, 931)
            || in_rect(80, 1080, 410, 1102)
            || in_rect(80, 1132, 1090, 1145)
            || in_rect(80, 1170, 970, 1183)
            || in_rect(80, 1208, 1050, 1221)
        {
            image::Rgb([55, 59, 67])
        } else if in_rect(80, 700, 1160, 704)
            || in_rect(80, 1000, 1160, 1004)
            || in_rect(80, 1320, 1160, 1324)
        {
            image::Rgb([174, 179, 187])
        } else if in_rect(80, 1390, 350, 1510)
            || in_rect(405, 1390, 675, 1510)
            || in_rect(730, 1390, 1160, 1510)
        {
            image::Rgb([229, 233, 239])
        } else {
            image::Rgb([246, 246, 242])
        }
    });
    image::DynamicImage::ImageRgb8(page)
}

/// The selected page as last rendered: its index, crop and rotation, the
/// image and its pixel size.
type DocumentPreview = (
    usize,
    crate::document_scan::Crop,
    crate::document_scan::Rotation,
    Image,
    (u32, u32),
);

/// What the sample scan feed was drawn for: the window size, and the guide's
/// centre and side.
type ScanViewKey = (u32, u32, i32, i32, u32);

/// Where newly captured pixels are projected.
enum Storage {
    /// Live: real files under this directory.
    Disk(PathBuf),
    /// Hermetic (screenshots/tests): in-memory procedural samples.
    Sample,
}

/// The reason a live viewfinder cannot be opened when a device was present.
/// This stays distinct from the ordinary "no camera connected" state so we
/// never mislead someone whose portal/device permission was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CameraIssue {
    /// Deterministic/headless equivalent of normal device discovery finding
    /// no camera. Runtime discovery represents this as `None` so retries can
    /// immediately reopen a newly attached device.
    NoDevice,
    PermissionDenied,
    StartFailed,
}

/// An in-progress video recording. Encoding is exclusively worker-owned; UI
/// retains only the start time needed to paint the elapsed indicator.
struct RecState {
    started: i64,
}

enum DocumentJobResult {
    Capture(Result<image::DynamicImage, String>),
    Recognize {
        page: usize,
        result: Result<tilo_ocr::Recognition, tilo_ocr::Error>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentJobKind {
    Capture,
    Recognize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OcrLanguage {
    English,
    German,
    French,
    Spanish,
}

impl OcrLanguage {
    const fn code(self) -> &'static str {
        match self {
            Self::English => "eng",
            Self::German => "deu",
            Self::French => "fra",
            Self::Spanish => "spa",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DocumentSettings {
    scan: tilo_capture::scanner::ScanSettings,
    ocr_language: OcrLanguage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentChoice {
    Scanner,
    Resolution,
    Color,
    Source,
    Language,
}

impl DocumentChoice {
    const fn index(self) -> usize {
        match self {
            Self::Scanner => 0,
            Self::Resolution => 1,
            Self::Color => 2,
            Self::Source => 3,
            Self::Language => 4,
        }
    }
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            scan: tilo_capture::scanner::ScanSettings {
                resolution_dpi: 300,
                color: tilo_capture::scanner::ScanColor::Color,
                source: tilo_capture::scanner::ScanSource::Flatbed,
            },
            ocr_language: OcrLanguage::English,
        }
    }
}

/// A local, physical-capture countdown.  It intentionally is not document
/// state: a peer changing a preference may replicate the preferred delay, but
/// must never arm or replay this device's shutter.
struct Countdown {
    due: Instant,
    mode: CaptureMode,
}

#[derive(Clone, Debug)]
enum Action {
    Nav(Screen),
    /// Dismiss an overlay screen (Settings) back to the viewfinder
    /// it was opened from.
    Back,
    Shutter,
    /// Hand off browsing to Photos. Camera never renders or mutates a
    /// collection browser itself.
    OpenPhotos,
    OpenRecentCapture,
    SetMode(CaptureMode),
    CycleFacing,
    DismissScan,
    Scan(usize),
    /// Choose an existing image through the system portal and scan it without
    /// turning Camera into a media browser.
    ImportScanImage,
    CaptureDocument,
    /// Choose option `index` of a document scan setting.
    SetDocumentChoice(DocumentChoice, usize),
    RotateDocument,
    RecognizeDocument,
    CopyDocumentText,
    ExportDocument,
    BeginDocumentCrop,
    ApplyDocumentCrop,
    CancelDocumentCrop,
    MoveDocumentPageEarlier,
    MoveDocumentPageLater,
    RemoveDocumentPage,
    CycleResolution,
    SetResolution(Resolution),
    SetPhotoFormat(PhotoFormat),
    SetTimer(Timer),
    ToggleGrid,
    ToggleLevel,
    ToggleMirror,
    ToggleGeotag,
    ToggleHdr,
    SetDefaultFacing(Facing),
    RetryCamera,
    OpenCameraPrivacySettings,
    OpenCameraChooser,
    SelectCamera(String),
}

#[derive(Clone, Debug)]
struct TouchTarget {
    id: u64,
    rect: Rect,
    action: Action,
    cancelled: bool,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated UI/lifecycle flag (the scan sheet was \
              dismissed, the frame needs a repaint, first sync \
              has run); grouping them would just add a field prefix at every call site in \
              this file with no behavioral benefit"
)]
pub struct CameraApp {
    rect: Rect,
    node: Option<NodeId>,

    storage: Storage,
    /// Where capture bytes live; documents carry only their `media_id`. The
    /// worker holds a clone.
    content: tilo_content::ContentStore,
    #[allow(
        dead_code,
        reason = "seed for screen; kept for restore after framework cutover"
    )]
    initial_screen: Screen,

    // CRDT-backed caches (refreshed every sync)
    screen: Screen,
    mode: CaptureMode,
    facing: Facing,
    settings: Settings,
    scans: Vec<ScanEntry>,

    // One contextual recent-capture handoff. Camera never owns a media library.
    captures: Vec<Capture>,

    // ephemeral UI state
    /// The viewfinder screen the Settings overlay was opened from,
    /// so its back button returns there rather than always to Camera.
    return_to: Screen,
    scan_dismissed: bool,
    scanned: Option<ScannedCode>,
    scan_gen: (u32, u32),
    toast: Option<String>,
    /// The toast on screen and when it appeared, so it can expire.
    toast_shown: Option<(String, Instant)>,
    last_mouse: Point,
    document_scan: crate::document_scan::ScanSession,
    document_settings: DocumentSettings,
    /// The scan settings, as standard dropdowns placed by the document view.
    document_controls: document_view::DocumentControls,
    /// The capture settings sheet shown over the viewfinder.
    settings_sheet: settings_sheet::SettingsSheet,
    /// Actions raised by the retained controls, applied after their event.
    ui_queue: settings_sheet::ActionQueue,
    /// The camera list a failed start offers, anchored on its action.
    chooser: Popover,
    chooser_anchor: Rect,
    tooltip: ui::widget::TooltipHost,
    /// Icon-only controls of the last frame and their names.
    tooltip_targets: Vec<(Rect, String)>,
    /// The theme of the last layout, for placing hosted controls.
    layout_theme: Theme,
    /// Whether the last layout tree holds the document controls.
    built_with_document: bool,
    scanner_devices: Vec<tilo_capture::scanner::Scanner>,
    selected_scanner: Option<String>,
    document_save_picker: Option<ui::portal::PortalChannel>,
    scan_image_picker: Option<ui::portal::PortalChannel>,
    document_crop_selection: ui::LocalState<CropSelection>,
    document_crop_editor: Option<ImageCropEditor>,
    document_crop_page: Option<usize>,
    document_preview: Option<DocumentPreview>,
    document_job: Option<mpsc::Receiver<DocumentJobResult>>,
    document_job_kind: Option<DocumentJobKind>,

    // In-progress video recording (None unless the shutter is armed in video
    // mode). The worker owns its encoder and gets coalesced latest frames.
    recording: Option<RecState>,
    video_finalizing: bool,
    countdown: Option<Countdown>,

    // Live camera frame (populated by the live-capture worker / tests).
    latest_frame: Option<FrameBuf>,
    frame_gen: u64,
    io_generation: u64,
    /// The shared preview pipeline; the recent-capture peek is requested from it.
    thumb_service: thumbnail::ThumbnailService,
    /// Captures whose preview is still being generated, by pipeline key.
    thumb_pending: HashMap<String, thumbnail::ThumbKey>,
    thumb_failed: HashSet<String>,
    scan_generation: u64,
    pending_scan: Option<(String, u64)>,
    worker: crate::worker::CameraWorker,
    pending_capture: Option<(tilo_actions::InvocationId, u64)>,
    pending_video: Option<(tilo_actions::InvocationId, u64)>,
    pending_scan_import: Option<tilo_actions::InvocationId>,
    pending_document_job: Option<tilo_actions::InvocationId>,
    pending_document_export: Option<tilo_actions::InvocationId>,
    completed_operations: HashMap<tilo_actions::InvocationId, tilo_actions::InvocationUpdate>,
    camera_issue: Option<CameraIssue>,
    selected_camera: Option<String>,
    /// The host supplies this preference before deciding whether to pump
    /// animation frames. Capture itself still runs; only decorative frame
    /// pumping is settled under reduced motion.
    motion_reduced: bool,
    #[cfg(feature = "live-capture")]
    camera: Option<crate::capture::Capture>,
    /// The `(device, width, height)` the live pipeline is currently open on,
    /// so device/resolution changes trigger a re-open.
    #[cfg(feature = "live-capture")]
    cam_config: Option<(String, u32, u32)>,

    // render caches
    cam_view: Option<(u32, u32, u8, Image)>,
    live_view: Option<(u64, Image)>,
    /// The sample scan feed for `(width, height, guide x, guide y, guide side)`.
    scan_view: Option<(ScanViewKey, Image)>,
    thumbs: HashMap<String, Image>,

    regions: Vec<(Rect, Action)>,
    active_touch: Option<TouchTarget>,
    dirty: bool,

    /// Framework-owned action dispatch: brackets the undo boundary, encodes and
    /// dispatches each action, and remembers the last one so a test can prove a
    /// button and its palette `ActionSpec` build the same `EncodedAction`.
    actions: crate::resource::ActionSink,
    search_provider: crate::search::CameraSearchProvider,
    /// The last-capture thumbnail invokes Photos through the shell's typed
    /// application-open boundary. Camera never imports Photos or serializes a
    /// side-channel payload; Photos receives only a verified portable hash.
    photos_handoff: Option<ActionEmitter<String>>,
    /// The unconditional Photos destination for browsing the user's library.
    /// Unlike `photos_handoff`, it carries no capture bytes because Photos
    /// owns its own collection and entry point.
    photos_browse: Option<ActionEmitter<()>>,
    video_handoff: Option<ActionEmitter<String>>,
}

impl Default for CameraApp {
    fn default() -> Self {
        Self::disk()
    }
}

impl CameraApp {
    /// The live app writes captures to the configured directory and opens on
    /// the viewfinder, with one contextual recent handoff a tap away.
    pub fn disk() -> Self {
        Self::with(Storage::Disk(library::resolve_dir()), Screen::Camera)
    }

    /// The hermetic app used by `--shots` and tests: self-contained sample
    /// content, no disk or device access.
    pub fn sample() -> Self {
        Self::with(Storage::Sample, Screen::Camera)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one constructor names every field of the app's state"
    )]
    fn with(storage: Storage, initial_screen: Screen) -> Self {
        let scanner_devices = match &storage {
            Storage::Sample => vec![tilo_capture::scanner::Scanner {
                device: "sample:document-scanner".into(),
                description: "Document scanner".into(),
            }],
            Storage::Disk(_) => tilo_capture::scanner::scanners().unwrap_or_default(),
        };
        let selected_scanner = scanner_devices
            .first()
            .map(|scanner| scanner.device.clone());
        // The hermetic sample keeps its bytes in memory, like everything else it does, and so do
        // unit tests that capture to a scratch library: they must not write the user's store.
        let content = match &storage {
            Storage::Sample => tilo_content::ContentStore::memory(),
            Storage::Disk(_) if cfg!(test) => tilo_content::ContentStore::memory(),
            Storage::Disk(_) => tilo_content::ContentStore::for_app(),
        };
        // Previews follow the same rule: the sample and tests keep them in memory.
        let thumb_service = match &storage {
            Storage::Disk(_) if !cfg!(test) => thumbnail::ThumbnailService::open_default(1)
                .unwrap_or_else(|_| thumbnail::ThumbnailService::ephemeral(1)),
            _ => thumbnail::ThumbnailService::ephemeral(1),
        };
        let ui_queue = settings_sheet::ActionQueue::default();
        let thumb_wake = ui::wake::WakeHandle::new();
        thumb_service.set_wake(move || thumb_wake.wake());
        Self {
            rect: Rect::ZERO,
            node: None,
            storage,
            worker: crate::worker::CameraWorker::new(content.clone()),
            content,
            initial_screen,
            screen: initial_screen,
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            settings: Settings::default(),
            scans: Vec::new(),
            captures: Vec::new(),
            return_to: Screen::Camera,
            scan_dismissed: false,
            scanned: None,
            scan_gen: (0, 0),
            toast: None,
            toast_shown: None,
            last_mouse: Point::new(-1_000.0, -1_000.0),
            document_scan: crate::document_scan::ScanSession::default(),
            document_settings: DocumentSettings::default(),
            document_controls: document_view::DocumentControls::new(&ui_queue),
            settings_sheet: settings_sheet::SettingsSheet::new(&ui_queue, &Settings::default()),
            ui_queue,
            chooser: Popover::new(),
            chooser_anchor: Rect::ZERO,
            tooltip: ui::widget::TooltipHost::new(),
            tooltip_targets: Vec::new(),
            layout_theme: Theme::dark(),
            built_with_document: false,
            scanner_devices,
            selected_scanner,
            document_save_picker: None,
            scan_image_picker: None,
            document_crop_selection: ui::LocalState::new(CropSelection::default()),
            document_crop_editor: None,
            document_crop_page: None,
            document_preview: None,
            document_job: None,
            document_job_kind: None,
            recording: None,
            video_finalizing: false,
            countdown: None,
            latest_frame: None,
            frame_gen: 0,
            io_generation: 0,
            thumb_service,
            thumb_pending: HashMap::new(),
            thumb_failed: HashSet::new(),
            scan_generation: 0,
            pending_scan: None,
            pending_capture: None,
            pending_video: None,
            pending_scan_import: None,
            pending_document_job: None,
            pending_document_export: None,
            completed_operations: HashMap::new(),
            camera_issue: None,
            selected_camera: None,
            motion_reduced: false,
            #[cfg(feature = "live-capture")]
            camera: None,
            #[cfg(feature = "live-capture")]
            cam_config: None,
            cam_view: None,
            live_view: None,
            scan_view: None,
            thumbs: HashMap::new(),
            regions: Vec::new(),
            active_touch: None,
            dirty: true,
            actions: crate::resource::ActionSink::default(),
            search_provider: crate::search::CameraSearchProvider::new(),
            photos_handoff: Self::media_handoff_emitter("so.tilo.photos", "Photos"),
            photos_browse: Self::photos_browse_emitter(),
            video_handoff: Self::media_handoff_emitter("so.tilo.video-player", "Video Player"),
        }
    }

    fn media_handoff_resource(app_id: &str, label: &str) -> tilo_actions::Resource {
        use tilo_actions::{
            ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
        };

        let launch_app_id = app_id.to_owned();
        let spec = ActionSpec::typed_operation(
            "open",
            format!("Open {label}"),
            format!("Open the latest capture in {label}"),
            OperationKind::Effect,
            crate::actions::APPLICATION_LAUNCH_TYPE,
            vec![ActionParam::required(
                "content_hash",
                ParamType::Text,
                "Verified portable BLAKE3 media hash",
            )],
            move |args| {
                EncodedAction::encode_operation(
                    crate::actions::APPLICATION_LAUNCH_TYPE,
                    &serde_json::json!({
                        "app_id": launch_app_id,
                        "content_hash": args.text("content_hash").unwrap_or(""),
                    }),
                )
                .unwrap_or_else(|_| EncodedAction {
                    type_id: crate::actions::APPLICATION_LAUNCH_TYPE.into(),
                    payload: Vec::new(),
                })
            },
        );
        Resource::new(format!("{app_id}.app"), "applications.app", label).with_action(spec)
    }

    fn media_handoff_emitter(app_id: &str, label: &str) -> Option<ActionEmitter<String>> {
        use tilo_actions::ActionBinding;

        let resource = Self::media_handoff_resource(app_id, label);
        let spec = resource.actions.first()?;
        ActionBinding::new(&resource, spec, |content_hash| {
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                "content_hash".into(),
                serde_json::Value::String(content_hash),
            );
            arguments
        })
        .ok()
        .map(ActionEmitter::new)
    }

    fn photos_browse_emitter() -> Option<ActionEmitter<()>> {
        use tilo_actions::ActionBinding;

        // Bind to Camera's exported capture root, which owns the routing
        // affordance. The action payload itself delegates to Photos, but the
        // host can still re-resolve this address against the current Camera
        // resource surface before permitting the launch.
        let resource = tilo_actions::Resource::new("camera.capture", "camera.capture", "Capture")
            .with_action(crate::actions::open_photos_action());
        ActionBinding::unit_for(&resource, "open-photos")
            .ok()
            .map(ActionEmitter::new)
    }

    /// Hand a freshly captured RGBA frame to the app (live-capture worker or
    /// tests). This is what the viewfinder shows, what the shutter saves, and
    /// — while recording — what the video encoder is fed.
    pub fn push_camera_frame(&mut self, frame: FrameBuf) -> bool {
        if !valid_frame(&frame) {
            self.camera_issue = Some(CameraIssue::StartFailed);
            self.toast = Some("Camera sent an unsupported frame".into());
            self.dirty = true;
            return false;
        }
        let Some(next_generation) = self.frame_gen.checked_add(1) else {
            // A generation is an authority token for the renderer cache. It
            // must never be reused: stale A→B frames need to stay inert even
            // at the theoretical counter boundary.
            self.camera_issue = Some(CameraIssue::StartFailed);
            self.toast = Some("Camera frame counter exhausted; restart Camera".into());
            self.dirty = true;
            return false;
        };
        if self.recording.is_some()
            && let Err(error) = self.worker.submit_recording_frame(frame.clone())
        {
            // Keep the viewfinder alive but make worker loss visible; stopping
            // will then return the same honest failure to an agent.
            self.toast = Some(error);
        }
        self.latest_frame = Some(frame);
        self.frame_gen = next_generation;
        self.dirty = true;
        true
    }

    /// True while a video recording is in progress.
    fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    fn is_counting_down(&self) -> bool {
        self.countdown.is_some()
    }

    /// The countdown is local device choreography, not a durable action.  A
    /// second shutter press is always a safe cancel; this avoids an accidental
    /// capture when someone changes their mind while the timer is visible.
    fn arm_or_cancel_timer(&mut self) -> Result<(), String> {
        if self.countdown.take().is_some() {
            self.toast = Some("Timer cancelled".into());
            self.dirty = true;
            return Ok(());
        }
        if !self.feed_active() {
            return Err(if self.mode == CaptureMode::Video {
                "Connect a camera to record"
            } else {
                "Connect a camera to capture"
            }
            .into());
        }
        let seconds = self.settings.timer.seconds();
        if seconds <= 0 {
            return self.fire_shutter();
        }
        self.countdown = Some(Countdown {
            due: ui::time::now() + Duration::from_secs(seconds as u64),
            mode: self.mode,
        });
        self.toast = Some(format!("Timer set for {seconds} seconds"));
        self.dirty = true;
        Ok(())
    }

    fn fire_shutter(&mut self) -> Result<(), String> {
        match self.mode {
            CaptureMode::Photo => self.capture(None),
            CaptureMode::Video if self.is_recording() => self.stop_recording(None),
            CaptureMode::Video => {
                self.start_recording();
                Ok(())
            }
        }
    }

    /// Complete a local countdown from `sync`, the lifecycle boundary that is
    /// also called by reduced-motion wakeups.  No peer action and no resource
    /// replay can reach this path.
    fn poll_countdown(&mut self) {
        let Some(countdown) = self.countdown.as_ref() else {
            return;
        };
        if ui::time::now() < countdown.due {
            self.dirty = true;
            return;
        }
        let mode = countdown.mode;
        self.countdown = None;
        // Mode is held with the timer, so a later document sync cannot make a
        // photo timer unexpectedly begin recording.
        if self.mode != mode {
            self.toast = Some("Timer cancelled because capture mode changed".into());
            return;
        }
        if let Err(error) = self.fire_shutter() {
            self.toast = Some(error);
        }
        self.dirty = true;
    }

    /// Begin a recording. Honest about missing hardware: with no live feed the
    /// disk/live source can't record, so it says so instead of faking a clip.
    fn start_recording(&mut self) {
        if !self.feed_active() {
            self.toast = Some("Connect a camera to record".into());
            return;
        }
        if self.video_finalizing {
            self.toast = Some("Finalizing the previous video…".into());
            return;
        }
        if let Err(error) = self.worker.start_recording() {
            self.toast = Some(error);
            return;
        }
        // Seed with the frame already on screen without converting/encoding
        // it on UI; the worker coalesces it with any arriving live frame.
        if let Some(fb) = &self.latest_frame
            && let Err(error) = self.worker.submit_recording_frame(fb.clone())
        {
            self.toast = Some(error);
            return;
        }
        self.recording = Some(RecState {
            started: now_secs(),
        });
        self.dirty = true;
    }

    /// Finish the recording by submitting its AVI mux/verification to the
    /// bounded finalizer. `sync` publishes the resulting canonical capture.
    fn stop_recording(
        &mut self,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> Result<(), String> {
        if self.recording.is_none() {
            return Err("No recording is in progress".into());
        }
        let Some(generation) = self.io_generation.checked_add(1) else {
            return Err("Camera job counter exhausted; restart Camera".into());
        };
        let dir = match &self.storage {
            Storage::Disk(dir) => native_paths().then(|| dir.clone()),
            Storage::Sample => None,
        };
        // AVI muxing can copy tens of MiB, including when screenshots/tests
        // use the hermetic sample storage. Never perform that work on the UI
        // action path.
        self.worker.finish_recording(crate::worker::VideoJob {
            invocation,
            generation,
            dir,
            facing: self.facing,
        })?;
        self.recording = None;
        self.io_generation = generation;
        self.video_finalizing = true;
        if let Some(invocation) = invocation {
            self.pending_video = Some((invocation, generation));
        }
        self.toast = Some("Finalizing video…".into());
        Ok(())
    }

    /// Whether a live viewfinder feed is currently available.
    fn feed_active(&self) -> bool {
        // A forced unavailable state is used by the hermetic screenshot/test
        // path. `Storage::Sample` normally supplies a procedural viewfinder,
        // but it must never obscure the permission/no-device recovery UI a
        // person will see in the live app. A disk source retains any real
        // frame that has already arrived; device lifecycle owns its recovery.
        if matches!(self.storage, Storage::Sample) && self.camera_issue.is_some() {
            return false;
        }
        match self.storage {
            Storage::Sample => true,
            Storage::Disk(_) => self.latest_frame.is_some(),
        }
    }

    fn no_feed_copy(&self) -> (&'static str, &'static str) {
        match self.camera_issue {
            Some(CameraIssue::NoDevice) | None => (
                "No camera connected",
                "Connect a camera to start the viewfinder",
            ),
            Some(CameraIssue::PermissionDenied) => (
                "Camera access needed",
                "Allow camera access in system settings, then try again",
            ),
            Some(CameraIssue::StartFailed) => (
                "Camera unavailable",
                "The connected camera could not be started. Try another camera or reconnect it",
            ),
        }
    }

    #[allow(
        clippy::unused_self,
        reason = "the live-capture implementation reads storage while the no-feature build must return no fabricated devices"
    )]
    fn available_camera_devices(&self) -> Vec<String> {
        #[cfg(feature = "live-capture")]
        {
            if matches!(self.storage, Storage::Disk(_)) {
                let mut devices = crate::capture::list_devices();
                devices.truncate(crate::actions::MAX_PUBLISHED_CAMERA_DEVICES);
                return devices;
            }
        }
        Vec::new()
    }

    /// Whether a real camera is open and delivering the viewfinder.
    fn viewfinder_live(&self) -> bool {
        #[cfg(feature = "live-capture")]
        {
            self.camera.is_some()
        }
        #[cfg(not(feature = "live-capture"))]
        {
            false
        }
    }

    /// Whether the live viewfinder has no camera yet on a platform whose
    /// devices appear only after the user is asked.
    fn awaiting_camera(&self) -> bool {
        matches!(self.storage, Storage::Disk(_))
            && self.screen == Screen::Camera
            && !self.viewfinder_live()
            && tilo_capture::availability(tilo_capture::Feature::Camera)
                == tilo_capture::Availability::Prompt
    }

    fn viewfinder_seed(facing: Facing) -> u8 {
        match facing {
            Facing::Back => 0,
            Facing::Front => 3,
            Facing::Wide => 2,
        }
    }

    fn push_region(&mut self, rect: Rect, action: Action) {
        self.regions.push((rect, action));
    }

    /// A stable semantic target for a hand-painted control. It is derived
    /// from the same `Action` the pointer hit map holds, so it cannot drift
    /// into a second accessibility-only command path.
    fn a11y_target(action: &Action) -> String {
        match action {
            Action::Nav(screen) => format!("camera.nav:{}", screen.as_str()),
            Action::Back => "camera.back".into(),
            Action::Shutter => "camera.shutter".into(),
            Action::OpenPhotos => "camera.open-photos".into(),
            Action::OpenRecentCapture => "camera.open-recent-capture".into(),
            Action::SetMode(mode) => format!("camera.mode:{}", mode.as_str()),
            Action::CycleFacing => "camera.facing".into(),
            Action::DismissScan => "camera.dismiss-scan".into(),
            Action::Scan(index) => format!("camera.scan:{index}"),
            Action::ImportScanImage => "camera.scan-image".into(),
            Action::CaptureDocument => "camera.document.capture".into(),
            Action::SetDocumentChoice(choice, index) => {
                format!("camera.document.choice:{}:{index}", choice.index())
            }
            Action::RotateDocument => "camera.document.rotate".into(),
            Action::RecognizeDocument => "camera.document.recognize".into(),
            Action::CopyDocumentText => "camera.document.copy-text".into(),
            Action::ExportDocument => "camera.document.export".into(),
            Action::BeginDocumentCrop => "camera.document.crop".into(),
            Action::ApplyDocumentCrop => "camera.document.crop.apply".into(),
            Action::CancelDocumentCrop => "camera.document.crop.cancel".into(),
            Action::MoveDocumentPageEarlier => "camera.document.move-earlier".into(),
            Action::MoveDocumentPageLater => "camera.document.move-later".into(),
            Action::RemoveDocumentPage => "camera.document.remove".into(),
            Action::CycleResolution => "camera.resolution".into(),
            Action::SetResolution(resolution) => {
                format!("camera.resolution:{}", resolution.as_str())
            }
            Action::SetPhotoFormat(format) => format!("camera.format:{}", format.as_str()),
            Action::SetTimer(timer) => format!("camera.timer:{}", timer.seconds()),
            Action::ToggleGrid => "camera.grid".into(),
            Action::ToggleLevel => "camera.level".into(),
            Action::ToggleMirror => "camera.mirror".into(),
            Action::ToggleGeotag => "camera.geotag".into(),
            Action::ToggleHdr => "camera.hdr".into(),
            Action::SetDefaultFacing(facing) => {
                format!("camera.default-facing:{}", facing.as_str())
            }
            Action::RetryCamera => "camera.retry-camera".into(),
            Action::OpenCameraPrivacySettings => "camera.open-camera-privacy".into(),
            Action::OpenCameraChooser => "camera.open-camera-chooser".into(),
            Action::SelectCamera(device) => format!("camera.select-camera:{device}"),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive mapping keeps every painted Camera action paired with its role, name, state, bounds, and stable semantic target; splitting the match would make that audit less reliable"
    )]
    fn a11y_node(&self, rect: Rect, action: &Action) -> AccessNode {
        let (role, name, state) = match action {
            Action::Nav(screen) => (
                Role::Button,
                format!("Open {}", screen.label()),
                AccessState::new().focusable(true),
            ),
            Action::Back => (
                Role::Button,
                "Back".into(),
                AccessState::new().focusable(true),
            ),
            Action::Shutter => (
                Role::Button,
                if self.is_counting_down() {
                    "Cancel capture timer"
                } else if self.is_recording() {
                    "Stop recording"
                } else if self.mode == CaptureMode::Video {
                    "Start recording"
                } else {
                    "Take photo"
                }
                .into(),
                AccessState::new().focusable(true),
            ),
            Action::OpenPhotos => (
                Role::Button,
                "Open Photos to browse your library".into(),
                AccessState::new().focusable(true),
            ),
            Action::OpenRecentCapture => (
                Role::Button,
                self.captures.first().map_or_else(
                    || "Latest capture".into(),
                    |capture| {
                        let (_, destination) =
                            crate::actions::capture_destination(capture.is_video());
                        format!("Open latest capture in {destination}")
                    },
                ),
                AccessState::new().focusable(true),
            ),
            Action::SetMode(mode) => (
                Role::RadioButton,
                format!("{} mode", mode.as_str()),
                AccessState::new()
                    .focusable(true)
                    .checked(self.mode == *mode),
            ),
            Action::CycleFacing => (
                Role::Button,
                format!("Switch camera, currently {}", self.facing.label()),
                AccessState::new().focusable(true),
            ),
            Action::DismissScan => (
                Role::Button,
                "Dismiss scan result".into(),
                AccessState::new().focusable(true),
            ),
            Action::Scan(_) => (
                Role::Button,
                "Use scanned result".into(),
                AccessState::new().focusable(true),
            ),
            Action::ImportScanImage => (
                Role::Button,
                "Choose an image to scan".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.scan_image_picker.is_some()),
            ),
            Action::CycleResolution => (
                Role::Button,
                format!(
                    "Change resolution, currently {}",
                    self.settings.resolution.label()
                ),
                AccessState::new().focusable(true),
            ),
            Action::SetResolution(resolution) => (
                Role::RadioButton,
                format!("{} resolution", resolution.label()),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.resolution == *resolution),
            ),
            Action::SetPhotoFormat(format) => (
                Role::RadioButton,
                format!("Save photos as {}", format.label()),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.photo_format == *format),
            ),
            Action::SetTimer(timer) => (
                Role::RadioButton,
                format!("{} timer", timer.label()),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.timer == *timer),
            ),
            Action::ToggleGrid => (
                Role::Switch,
                "Composition grid".into(),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.grid),
            ),
            Action::ToggleLevel => (
                Role::Switch,
                "Level guide".into(),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.level),
            ),
            Action::ToggleMirror => (
                Role::Switch,
                "Mirror front camera".into(),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.mirror_front),
            ),
            Action::ToggleGeotag => (
                Role::Switch,
                "Save location with photos".into(),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.geotag),
            ),
            Action::ToggleHdr => (
                Role::Switch,
                "HDR".into(),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.hdr),
            ),
            Action::SetDefaultFacing(facing) => (
                Role::RadioButton,
                format!("Start with the {} camera", facing.label().to_lowercase()),
                AccessState::new()
                    .focusable(true)
                    .checked(self.settings.default_facing == *facing),
            ),
            Action::RetryCamera => (
                Role::Button,
                "Refresh available cameras".into(),
                AccessState::new().focusable(true),
            ),
            Action::OpenCameraPrivacySettings => (
                Role::Button,
                "Open camera privacy settings".into(),
                AccessState::new().focusable(true),
            ),
            Action::OpenCameraChooser => (
                Role::Button,
                "Choose a camera".into(),
                AccessState::new().focusable(true),
            ),
            Action::SelectCamera(device) => (
                Role::RadioButton,
                format!("Use {device}"),
                AccessState::new()
                    .focusable(true)
                    .checked(self.selected_camera.as_deref() == Some(device.as_str())),
            ),
            Action::CaptureDocument => (
                Role::Button,
                "Scan document page".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.selected_scanner.is_none() || self.document_job.is_some()),
            ),
            Action::SetDocumentChoice(choice, index) => (
                Role::RadioButton,
                document_view::choice_labels(*choice)
                    .get(*index)
                    .map_or_else(
                        || format!("Option {}", index + 1),
                        |label| (*label).to_owned(),
                    ),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_job.is_some()),
            ),
            Action::RotateDocument => (
                Role::Button,
                "Rotate selected page clockwise".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_scan.pages.is_empty() || self.document_job.is_some()),
            ),
            Action::RecognizeDocument => (
                Role::Button,
                "Recognize text on selected page".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_scan.pages.is_empty() || self.document_job.is_some()),
            ),
            Action::CopyDocumentText => (
                Role::Button,
                "Copy recognized text from selected page".into(),
                AccessState::new().focusable(true).disabled(
                    self.document_job.is_some()
                        || self
                            .document_scan
                            .pages
                            .get(self.document_scan.selected)
                            .and_then(|page| page.ocr.as_ref())
                            .is_none(),
                ),
            ),
            Action::ExportDocument => (
                Role::Button,
                "Save searchable PDF".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_scan.pages.is_empty() || self.document_job.is_some()),
            ),
            Action::BeginDocumentCrop => (
                Role::Button,
                "Crop selected page".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_scan.pages.is_empty() || self.document_job.is_some()),
            ),
            Action::ApplyDocumentCrop => (
                Role::Button,
                "Apply crop".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_crop_editor.is_none()),
            ),
            Action::CancelDocumentCrop => (
                Role::Button,
                "Cancel crop".into(),
                AccessState::new().focusable(true),
            ),
            Action::MoveDocumentPageEarlier => (
                Role::Button,
                "Move selected page earlier".into(),
                AccessState::new().focusable(true).disabled(
                    self.document_job.is_some()
                        || self.document_scan.pages.is_empty()
                        || self.document_scan.selected == 0,
                ),
            ),
            Action::MoveDocumentPageLater => (
                Role::Button,
                "Move selected page later".into(),
                AccessState::new().focusable(true).disabled(
                    self.document_job.is_some()
                        || self.document_scan.pages.is_empty()
                        || self.document_scan.selected + 1 >= self.document_scan.pages.len(),
                ),
            ),
            Action::RemoveDocumentPage => (
                Role::Button,
                "Remove selected page".into(),
                AccessState::new()
                    .focusable(true)
                    .disabled(self.document_scan.pages.is_empty() || self.document_job.is_some()),
            ),
        };
        AccessNode::new(role)
            .with_name(name)
            .with_state(state)
            .with_bounds(rect)
            .with_action_target(Self::a11y_target(action))
    }

    /// Dispatch one Camera [`CameraAction`](crate::actions::CameraAction) — the
    /// single mutation path shared with the Ctrl+K palette. The action is applied
    /// Show the capture settings, scans, and recent handoff the framework resource holds.
    /// An empty projection does not wipe preferences or a handoff that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::CameraState) {
        let untouched = state.scans.is_empty()
            && state.recent.is_none()
            && state.mode == CaptureMode::Photo
            && state.facing == Facing::Back
            && state.settings == Settings::default();
        let local_unpublished = !self.scans.is_empty()
            || !self.captures.is_empty()
            || self.mode != CaptureMode::Photo
            || self.facing != Facing::Back
            || self.settings != Settings::default();
        if untouched && local_unpublished {
            return;
        }
        if self.recording.is_none() {
            self.mode = state.mode;
        }
        self.facing = state.facing;
        self.settings = state.settings;
        self.scans.clone_from(&state.scans);
        if matches!(self.storage, Storage::Disk(_)) {
            let next = state.recent.as_ref().map(|shared| Capture {
                id: shared.id.clone(),
                kind: shared.kind,
                ts: shared.ts,
                w: shared.w,
                h: shared.h,
                dur_ms: shared.dur_ms,
                label: shared.label.clone(),
                facing: shared.facing,
                scene: 0,
                path: None,
                media_id: Some(shared.media_id.clone()),
            });
            let changed = self.captures.first().map(|capture| &capture.id)
                != next.as_ref().map(|capture| &capture.id);
            self.captures = next.into_iter().collect();
            if changed {
                self.thumb_pending.clear();
            }
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Camera>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn scan_count_for_test(&self) -> usize {
        self.scans.len()
    }

    /// Dispatch one Camera [`CameraAction`](crate::actions::CameraAction) — the
    /// single mutation path shared with the Ctrl+K palette. The action is applied
    /// through the Tilo action executor (a pinned actor + committed change, so it
    /// replays byte-identically on peers), exactly as the palette does. The doc
    /// heads captured just before the first action of the event are stashed so the
    /// host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)).
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::CameraAction) {
        if let Err(error) = self.try_perform_action(doc, action) {
            self.toast = Some(format!("Couldn't save change: {error}"));
        }
    }

    /// Dispatch where the caller must not continue after a refused canonical
    /// mutation (worker settlement and destructive resource effects).
    fn try_perform_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::CameraAction,
    ) -> Result<(), String> {
        self.actions
            .try_dispatch_with_context(
                doc,
                crate::actions::CAMERA_ACTION_TYPE,
                action,
                tilo_actions::ActionContext::for_action("so.tilo.camera", "Camera", action),
            )
            .map_err(|error| error.to_string())
    }

    /// The compact context Ctrl+K needs to build Camera's canonical resources.
    pub(crate) fn camera_resource_ctx(&self) -> crate::actions::CameraResourceCtx {
        self.camera_resource_ctx_with_recent(self.captures.first().map(Self::capture_item))
    }

    fn capture_item(capture: &Capture) -> crate::actions::CaptureItem {
        crate::actions::CaptureItem {
            id: capture.id.clone(),
            label: capture.label.clone(),
            is_video: capture.is_video(),
            media_id: capture.media_id.clone(),
        }
    }

    fn camera_resource_ctx_with_recent(
        &self,
        recent: Option<crate::actions::CaptureItem>,
    ) -> crate::actions::CameraResourceCtx {
        let camera_devices = self.available_camera_devices();
        crate::actions::CameraResourceCtx {
            recent,
            mode: self.mode,
            facing: self.facing,
            recording: self.is_recording(),
            feed_available: self.feed_active(),
            camera_permission_denied: self.camera_issue == Some(CameraIssue::PermissionDenied),
            camera_devices,
            selected_camera: self.selected_camera.clone(),
            settings: self.settings,
            scan_import_pending: self.scan_image_picker.is_some(),
            document_scanner_available: self.selected_scanner.as_ref().is_some_and(|selected| {
                self.scanner_devices
                    .iter()
                    .any(|scanner| scanner.device == *selected)
            }),
            document_pages: self.document_scan.pages.len(),
            document_selected: self.document_scan.selected,
            document_busy: self.document_job.is_some(),
            document_has_text: self
                .document_scan
                .pages
                .get(self.document_scan.selected)
                .is_some_and(|page| page.ocr.is_some()),
        }
    }

    pub(crate) fn resolve_camera_resource(
        &self,
        doc: &AutoCommit,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        if matches!(
            id,
            "camera.capture"
                | "camera.scanner"
                | "camera.document-scanner"
                | "camera.settings"
                | "camera"
        ) {
            return crate::actions::resolve_resource(&self.camera_resource_ctx(), doc, id);
        }
        let (capture_id, expect_video) = id
            .strip_prefix("camera.photo:")
            .map(|capture_id| (capture_id, false))
            .or_else(|| {
                id.strip_prefix("camera.video:")
                    .map(|capture_id| (capture_id, true))
            })?;
        let item = self
            .captures
            .iter()
            .find(|capture| capture.id == capture_id && capture.is_video() == expect_video)
            .map(Self::capture_item)?;
        crate::actions::resolve_resource(&self.camera_resource_ctx_with_recent(Some(item)), doc, id)
    }

    fn refresh_search_provider(&mut self) {
        if let Err(error) = self.search_provider.sync(&self.camera_resource_ctx()) {
            self.toast = Some(format!("Camera search kept its previous actions: {error}"));
        }
    }

    fn apply(&mut self, action: Action, doc: &mut AutoCommit, root: &ObjId) {
        self.dirty = true;
        // Any interaction clears a transient confirmation; the action below
        // sets a fresh one if it has something to report.
        self.toast = None;
        match action {
            Action::Nav(s) => self.apply_nav(s, doc, root),
            Action::Back => self.apply_back(doc, root),
            Action::Shutter => self.apply_shutter(doc, root),
            Action::OpenPhotos => self.open_photos(),
            Action::OpenRecentCapture => self.open_recent_capture(),
            Action::SetMode(m) => self.apply_set_mode(m, doc, root),
            Action::CycleFacing => self.apply_cycle_facing(doc, root),
            Action::DismissScan => self.scan_dismissed = true,
            Action::Scan(i) => self.apply_scan_action(i, doc, root),
            Action::ImportScanImage => self.request_scan_image(),
            Action::RetryCamera => self.retry_camera(),
            Action::OpenCameraPrivacySettings => self.open_camera_privacy_settings(),
            Action::OpenCameraChooser => self.open_camera_chooser(),
            Action::SelectCamera(device) => self.select_camera(&device),
            Action::CaptureDocument => self.capture_document_page(),
            Action::SetDocumentChoice(choice, index) => self.apply_document_choice(choice, index),
            Action::RotateDocument => self.rotate_document_page(),
            Action::RecognizeDocument => self.recognize_document_page(),
            Action::CopyDocumentText => self.copy_document_text(),
            Action::ExportDocument => self.request_document_export(),
            Action::BeginDocumentCrop => self.begin_document_crop(),
            Action::ApplyDocumentCrop => self.apply_document_crop(),
            Action::CancelDocumentCrop => self.cancel_document_crop(),
            Action::MoveDocumentPageEarlier => self.move_document_page(false),
            Action::MoveDocumentPageLater => self.move_document_page(true),
            Action::RemoveDocumentPage => self.remove_document_page(),
            settings_action @ (Action::CycleResolution
            | Action::SetResolution(_)
            | Action::SetPhotoFormat(_)
            | Action::SetTimer(_)
            | Action::ToggleGrid
            | Action::ToggleLevel
            | Action::ToggleMirror
            | Action::ToggleGeotag
            | Action::ToggleHdr
            | Action::SetDefaultFacing(_)) => {
                self.apply_settings_toggle(&settings_action, doc, root);
            }
        }
        self.present_controls();
    }

    /// Forget a toast that has been on screen for its time.
    fn expire_toast(&mut self) {
        let now = ui::time::now();
        match (&self.toast, &self.toast_shown) {
            (Some(text), Some((shown, since))) if text == shown => {
                if now.saturating_duration_since(*since) >= ui::widget::toast::DEFAULT_TTL {
                    self.toast = None;
                    self.toast_shown = None;
                }
            }
            (Some(text), _) => self.toast_shown = Some((text.clone(), now)),
            (None, _) => self.toast_shown = None,
        }
    }

    /// The screen under the settings sheet, or the screen itself.
    fn base_screen(&self) -> Screen {
        if self.screen == Screen::Settings {
            self.return_to
        } else {
            self.screen
        }
    }

    /// Apply the actions the hosted controls raised during the last event.
    fn drain_ui_queue(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        let actions: Vec<Action> = self.ui_queue.borrow_mut().drain(..).collect();
        for action in actions {
            self.apply(action, doc, root);
        }
    }

    /// Show the current settings in the hosted controls, and the settings
    /// sheet while it is the screen.
    fn present_controls(&mut self) {
        self.settings_sheet.present(&self.settings);
        let open = self.screen == Screen::Settings;
        if self.settings_sheet.dialog.is_open() != open {
            self.settings_sheet.dialog.set_open(open);
        }
        self.present_document_controls();
    }

    fn apply_nav(&mut self, s: Screen, _doc: &mut AutoCommit, _root: &ObjId) {
        // Remember which viewfinder an overlay was summoned from so its
        // back button returns there.
        if matches!(s, Screen::Settings)
            && matches!(
                self.screen,
                Screen::Camera | Screen::Scanner | Screen::Document
            )
        {
            self.return_to = self.screen;
        }
        self.screen = s;
    }

    fn capture_document_page(&mut self) {
        if self.document_job.is_some() {
            return;
        }
        self.cancel_document_crop();
        let Some(scanner_device) = self.selected_scanner.as_ref().and_then(|selected| {
            self.scanner_devices
                .iter()
                .find(|scanner| scanner.device == *selected)
                .map(|scanner| scanner.device.clone())
        }) else {
            self.toast = Some(if self.scanner_devices.is_empty() {
                "Connect a document scanner to begin".into()
            } else {
                "Choose which scanner to use".into()
            });
            return;
        };
        match self.storage {
            Storage::Sample => {
                self.document_scan.add_page(sample_document_image());
                self.toast = Some(format!("Page {} captured", self.document_scan.pages.len()));
            }
            Storage::Disk(_) => {
                let (sender, receiver) = mpsc::channel();
                self.document_job = Some(receiver);
                self.document_job_kind = Some(DocumentJobKind::Capture);
                let settings = self.document_settings.scan;
                let spawn = ui::rt::spawn_thread("camera-document-capture", move || {
                    let result = tilo_capture::scanner::scan_page(&scanner_device, settings)
                        .and_then(|bytes| {
                            image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                                .map_err(|error| {
                                    format!("scanner returned an invalid PNG: {error}")
                                })
                        });
                    let _ = sender.send(DocumentJobResult::Capture(result));
                });
                if let Err(error) = spawn {
                    self.document_job = None;
                    self.document_job_kind = None;
                    self.toast = Some(format!("Could not start scanner: {error}"));
                }
            }
        }
    }

    fn apply_document_choice(&mut self, choice: DocumentChoice, index: usize) {
        if self.document_job.is_some() {
            return;
        }
        match choice {
            DocumentChoice::Scanner => {
                if let Some(scanner) = self.scanner_devices.get(index) {
                    self.selected_scanner = Some(scanner.device.clone());
                    self.toast = Some(format!("Using {}", scanner.description));
                }
            }
            DocumentChoice::Resolution => {
                if let Some(value) = [150, 300, 600].get(index) {
                    self.document_settings.scan.resolution_dpi = *value;
                }
            }
            DocumentChoice::Color => {
                if let Some(value) = [
                    tilo_capture::scanner::ScanColor::Color,
                    tilo_capture::scanner::ScanColor::Grayscale,
                    tilo_capture::scanner::ScanColor::LineArt,
                ]
                .get(index)
                {
                    self.document_settings.scan.color = *value;
                }
            }
            DocumentChoice::Source => {
                if let Some(value) = [
                    tilo_capture::scanner::ScanSource::Flatbed,
                    tilo_capture::scanner::ScanSource::Feeder,
                    tilo_capture::scanner::ScanSource::FeederDuplex,
                ]
                .get(index)
                {
                    self.document_settings.scan.source = *value;
                }
            }
            DocumentChoice::Language => {
                if let Some(value) = [
                    OcrLanguage::English,
                    OcrLanguage::German,
                    OcrLanguage::French,
                    OcrLanguage::Spanish,
                ]
                .get(index)
                {
                    self.document_settings.ocr_language = *value;
                }
            }
        }
    }

    fn rotate_document_page(&mut self) {
        if self.document_job.is_some() {
            return;
        }
        self.cancel_document_crop();
        let Some(page) = self
            .document_scan
            .pages
            .get_mut(self.document_scan.selected)
        else {
            return;
        };
        page.rotate_clockwise();
        self.toast = Some("Page rotated; recognize text again when ready".into());
    }

    fn recognize_document_page(&mut self) {
        if self.document_job.is_some() {
            return;
        }
        let index = self.document_scan.selected;
        let Some(image) = self
            .document_scan
            .pages
            .get(index)
            .map(crate::document_scan::Page::rendered)
        else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        self.document_job = Some(receiver);
        self.document_job_kind = Some(DocumentJobKind::Recognize);
        let language = self.document_settings.ocr_language.code();
        let spawn = ui::rt::spawn_thread("camera-document-ocr", move || {
            let result = tilo_ocr::recognize_detailed(&image, language);
            let _ = sender.send(DocumentJobResult::Recognize {
                page: index,
                result,
            });
        });
        if spawn.is_err() {
            self.document_job = None;
            self.document_job_kind = None;
            self.toast = Some("Text recognition could not start. Try again in a moment.".into());
        }
    }

    fn copy_document_text(&mut self) {
        let Some(document) = self
            .document_scan
            .pages
            .get(self.document_scan.selected)
            .and_then(|page| page.ocr.as_ref())
        else {
            return;
        };
        let text = document.plain_text();
        if text.trim().is_empty() {
            self.toast = Some("No recognized text to copy".into());
            return;
        }
        ui::clipboard_copy(&text);
        self.toast = Some("Recognized text copied".into());
    }

    fn document_text_status(&self) -> String {
        if self.document_job_kind == Some(DocumentJobKind::Recognize) {
            return "Reading text…".into();
        }
        let Some(document) = self
            .document_scan
            .pages
            .get(self.document_scan.selected)
            .and_then(|page| page.ocr.as_ref())
        else {
            return "Text not recognized".into();
        };
        if document.language != self.document_settings.ocr_language.code() {
            return "Language changed; update text".into();
        }
        let count = document.words.len();
        let summary = if count == 1 {
            "1 word recognized".to_owned()
        } else {
            format!("{count} words recognized")
        };
        self.document_scan
            .pages
            .get(self.document_scan.selected)
            .and_then(|page| page.ocr_deskew_degrees)
            .map_or_else(
                || summary.clone(),
                |angle| format!("{summary}, straightened by {:.1}°", angle.abs()),
            )
    }

    fn poll_document_job(&mut self) {
        let Some(receiver) = self.document_job.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.document_job = None;
                self.document_job_kind = None;
                self.toast = Some("Document processing stopped unexpectedly".into());
                if let Some(invocation) = self.pending_document_job.take() {
                    self.completed_operations.insert(
                        invocation,
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "camera.document_worker_stopped".into(),
                                message: "Document processing stopped unexpectedly".into(),
                                details: None,
                                retryable: true,
                            },
                        },
                    );
                }
                return;
            }
        };
        self.document_job = None;
        self.document_job_kind = None;
        let mut terminal_error = None;
        match result {
            DocumentJobResult::Capture(Ok(image)) => {
                self.document_scan.add_page(image);
                self.toast = Some(format!("Page {} captured", self.document_scan.pages.len()));
            }
            DocumentJobResult::Capture(Err(error)) => {
                self.toast = Some(crate::document_scan::capture_error_message(&error).into());
                terminal_error = Some(("camera.document_capture_failed", error));
            }
            DocumentJobResult::Recognize {
                page,
                result: Ok(recognition),
            } => {
                let words = recognition.document.words.len();
                if let Some(target) = self.document_scan.pages.get_mut(page) {
                    target.ocr = Some(recognition.document);
                    target.ocr_deskew_degrees = recognition.deskew_degrees;
                    self.toast = Some(recognition.deskew_degrees.map_or_else(
                        || format!("Recognized {words} words"),
                        |angle| {
                            format!(
                                "Recognized {words} words, straightened by {:.1}°",
                                angle.abs()
                            )
                        },
                    ));
                }
            }
            DocumentJobResult::Recognize {
                result: Err(error), ..
            } => {
                self.toast = Some(error.user_message().into());
                terminal_error = Some(("camera.document_ocr_failed", error.user_message().into()));
            }
        }
        if let Some(invocation) = self.pending_document_job.take() {
            let update = terminal_error.map_or_else(
                || tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                |(code, message)| tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: code.into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                },
            );
            self.completed_operations.insert(invocation, update);
        }
    }

    fn request_document_export(&mut self) {
        if self.document_scan.pages.is_empty() || self.document_save_picker.is_some() {
            return;
        }
        self.document_save_picker = Some(
            ui::portal::PickRequest::save("Save scanned document", "Save", "Scanned document.pdf")
                .thread_name("camera-document-save-picker")
                .send(),
        );
    }

    fn begin_document_crop(&mut self) {
        if self.document_job.is_some() {
            return;
        }
        let index = self.document_scan.selected;
        let Some(page) = self.document_scan.pages.get(index) else {
            return;
        };
        let (rgba, width, height, selection) = page.crop_editor_source();
        self.document_crop_selection = ui::LocalState::new(selection);
        self.document_crop_editor = Some(ImageCropEditor::new(
            rgba,
            width,
            height,
            self.document_crop_selection.clone(),
        ));
        self.document_crop_page = Some(index);
    }

    fn apply_document_crop(&mut self) {
        let Some(index) = self.document_crop_page else {
            return;
        };
        let selection = self.document_crop_selection.get();
        self.toast = Some(
            match self
                .document_scan
                .pages
                .get_mut(index)
                .ok_or("The selected page is no longer available")
                .and_then(|page| page.set_normalized_crop(selection))
            {
                Ok(()) => "Crop applied; recognize text again when ready".into(),
                Err(error) => error.into(),
            },
        );
        self.cancel_document_crop();
    }

    fn cancel_document_crop(&mut self) {
        self.document_crop_editor = None;
        self.document_crop_page = None;
    }

    fn poll_document_export(&mut self) {
        let Some(receiver) = self.document_save_picker.as_ref() else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::PortalOutcome::Failed("The file picker stopped responding".into())
            }
        };
        self.document_save_picker = None;
        let mut terminal_error = None;
        let mut cancelled = false;
        match outcome {
            ui::portal::PortalOutcome::Opened(mut path) => {
                if path.extension().is_none() {
                    path.set_extension("pdf");
                }
                self.toast = Some(
                    match crate::files::save(&path, &self.document_scan.searchable_pdf()) {
                        Ok(()) => format!(
                            "Saved {}",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("scanned document")
                        ),
                        Err(error) => {
                            terminal_error = Some(error.to_string());
                            format!("Could not save document: {error}")
                        }
                    },
                );
            }
            ui::portal::PortalOutcome::Cancelled => cancelled = true,
            ui::portal::PortalOutcome::Failed(error) => {
                self.toast = Some(format!("Could not open save location: {error}"));
                terminal_error = Some(error);
            }
        }
        if let Some(invocation) = self.pending_document_export.take() {
            let update = if cancelled {
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("document export cancelled".into()),
                }
            } else if let Some(message) = terminal_error {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "camera.document_export_failed".into(),
                        message,
                        details: None,
                        retryable: true,
                    },
                }
            } else {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            };
            self.completed_operations.insert(invocation, update);
        }
    }

    fn move_document_page(&mut self, later: bool) {
        if self.document_job.is_some() {
            return;
        }
        self.cancel_document_crop();
        let from = self.document_scan.selected;
        let to = if later {
            from.saturating_add(1)
        } else {
            from.saturating_sub(1)
        };
        if self.document_scan.move_page(from, to) {
            self.toast = Some(format!("Page moved to position {}", to + 1));
        }
    }

    fn remove_document_page(&mut self) {
        if self.document_job.is_some() {
            return;
        }
        self.cancel_document_crop();
        let index = self.document_scan.selected;
        if self.document_scan.remove_page(index) {
            self.toast = Some("Page removed".into());
        }
    }

    fn apply_back(&mut self, _doc: &mut AutoCommit, _root: &ObjId) {
        self.screen = self.return_to;
    }

    fn apply_shutter(&mut self, _doc: &mut AutoCommit, _root: &ObjId) {
        if let Err(error) = self.arm_or_cancel_timer() {
            self.toast = Some(error);
        }
    }

    fn open_recent_capture(&mut self) {
        let Some(capture) = self.captures.first() else {
            self.toast = Some("Take a capture before opening it".into());
            return;
        };
        let media_id = capture.media_id.clone().or_else(|| {
            capture
                .path
                .as_deref()
                .and_then(|path| crate::media::store_file(&self.content, path))
        });
        let Some(media_id) = media_id else {
            self.toast = Some("Latest capture is not available as portable media".into());
            return;
        };
        let Some(hash) = media_id.strip_prefix(crate::media::MEDIA_PREFIX) else {
            self.toast = Some("Latest capture has an invalid portable media identity".into());
            return;
        };
        if crate::media::read(&self.content, &media_id).is_none() {
            self.toast = Some("Latest capture bytes are unavailable on this device".into());
            return;
        }
        let (_, destination) = crate::actions::capture_destination(capture.is_video());
        let handoff = if capture.is_video() {
            &mut self.video_handoff
        } else {
            &mut self.photos_handoff
        };
        let Some(handoff) = handoff.as_mut() else {
            self.toast = Some(format!(
                "{destination} handoff is unavailable in this Camera session"
            ));
            return;
        };
        match handoff.emit(hash.to_owned()) {
            Ok(()) => self.toast = Some(format!("Opening latest capture in {destination}…")),
            Err(error) => self.toast = Some(format!("Couldn't open {destination}: {error}")),
        }
    }

    /// Open Photos' own library entry point. This intentionally accepts no
    /// Camera-owned media id: browsing is Photos' concern, whereas the recent
    /// tile above is the explicit verified-capture handoff.
    fn open_photos(&mut self) {
        let Some(handoff) = self.photos_browse.as_mut() else {
            self.toast = Some("Photos is unavailable in this Camera session".into());
            return;
        };
        match handoff.emit(()) {
            Ok(()) => self.toast = Some("Opening Photos…".into()),
            Err(error) => self.toast = Some(format!("Couldn't open Photos: {error}")),
        }
    }

    fn apply_set_mode(&mut self, m: CaptureMode, doc: &mut AutoCommit, _root: &ObjId) {
        // Switching mode mid-record would strand the clip; ignore.
        if !self.is_recording() && !self.is_counting_down() {
            self.mode = m;
            self.perform_action(doc, &crate::actions::CameraAction::SetMode { mode: m });
            // Photo/Video also bring the viewfinder forward when chosen
            // from the scanner's shared switcher.
            if self.screen != Screen::Camera {
                self.screen = Screen::Camera;
            }
        }
    }

    fn apply_cycle_facing(&mut self, doc: &mut AutoCommit, _root: &ObjId) {
        self.facing = self.facing.next();
        self.perform_action(
            doc,
            &crate::actions::CameraAction::SetFacing {
                facing: self.facing,
            },
        );
        self.cam_view = None;
    }

    fn apply_scan_action(&mut self, i: usize, doc: &mut AutoCommit, root: &ObjId) {
        if let Some(s) = self.scanned.clone() {
            // The chosen action (open / connect / copy) is external; the
            // scan-history row-insert is the collaborative action.
            self.perform_scan_action(&s, i);
            self.perform_action(
                doc,
                &crate::actions::CameraAction::RecordScan {
                    raw: s.raw.clone(),
                    ts: now_secs(),
                },
            );
            self.scans = doc::read_scans(doc, root);
        }
    }

    fn apply_settings_toggle(&mut self, action: &Action, doc: &mut AutoCommit, root: &ObjId) {
        let camera_action = match action {
            Action::CycleResolution => {
                self.settings.resolution = cycle(&Resolution::ALL, self.settings.resolution);
                crate::actions::CameraAction::SetResolution {
                    resolution: self.settings.resolution,
                }
            }
            Action::SetResolution(resolution) => {
                self.settings.resolution = *resolution;
                crate::actions::CameraAction::SetResolution {
                    resolution: *resolution,
                }
            }
            Action::SetPhotoFormat(format) => {
                self.settings.photo_format = *format;
                crate::actions::CameraAction::SetPhotoFormat {
                    photo_format: *format,
                }
            }
            Action::SetTimer(tm) => {
                self.settings.timer = *tm;
                crate::actions::CameraAction::SetTimer { timer: *tm }
            }
            Action::ToggleGrid => {
                self.settings.grid = !self.settings.grid;
                crate::actions::CameraAction::SetGrid {
                    on: self.settings.grid,
                }
            }
            Action::ToggleLevel => {
                self.settings.level = !self.settings.level;
                crate::actions::CameraAction::SetLevel {
                    on: self.settings.level,
                }
            }
            Action::ToggleMirror => {
                self.settings.mirror_front = !self.settings.mirror_front;
                crate::actions::CameraAction::SetMirrorFront {
                    on: self.settings.mirror_front,
                }
            }
            Action::ToggleGeotag => {
                self.settings.geotag = !self.settings.geotag;
                crate::actions::CameraAction::SetGeotag {
                    on: self.settings.geotag,
                }
            }
            Action::ToggleHdr => {
                self.settings.hdr = !self.settings.hdr;
                crate::actions::CameraAction::SetHdr {
                    on: self.settings.hdr,
                }
            }
            Action::SetDefaultFacing(facing) => {
                self.settings.default_facing = *facing;
                crate::actions::CameraAction::SetDefaultFacing { facing: *facing }
            }
            _ => return,
        };
        self.perform_action(doc, &camera_action);
        self.settings = doc::read_settings(doc, root);
    }

    /// Take a photo: write the current frame to disk (live) or append an
    /// in-memory sample (hermetic).
    fn capture(&mut self, invocation: Option<tilo_actions::InvocationId>) -> Result<(), String> {
        match &self.storage {
            Storage::Sample => {
                let id = format!(
                    "IMG_{}.{}",
                    now_secs(),
                    self.settings.photo_format.extension()
                );
                let cap = Capture {
                    id,
                    kind: CaptureKind::Photo,
                    ts: now_secs(),
                    w: self.settings.resolution.dims().0,
                    h: self.settings.resolution.dims().1,
                    dur_ms: 0,
                    label: "New capture".into(),
                    facing: self.facing,
                    scene: Self::viewfinder_seed(self.facing),
                    path: None,
                    media_id: None,
                };
                self.captures.clear();
                self.captures.push(cap);
                self.toast = Some("Photo saved".into());
                Ok(())
            }
            Storage::Disk(dir) => {
                let Some(frame) = self.latest_frame.clone() else {
                    return Err("Connect a camera to capture".into());
                };
                let generation = self
                    .io_generation
                    .checked_add(1)
                    .ok_or_else(|| "Camera job counter exhausted; restart Camera".to_string())?;
                self.worker.submit(crate::worker::PhotoJob {
                    invocation,
                    generation,
                    dir: native_paths().then(|| dir.clone()),
                    frame,
                    format: self.settings.photo_format,
                    facing: self.facing,
                })?;
                self.io_generation = generation;
                if let Some(invocation) = invocation {
                    self.pending_capture = Some((invocation, generation));
                }
                self.toast = Some("Saving photo…".into());
                Ok(())
            }
        }
    }

    fn perform_resource_operation(
        &mut self,
        operation: crate::actions::CameraOperation,
    ) -> Result<(), String> {
        use crate::actions::CameraOperation;
        match operation {
            CameraOperation::Reveal { screen } => self.screen = screen,
            CameraOperation::Capture => self.capture(None)?,
            CameraOperation::ToggleRecording => {
                if self.is_recording() {
                    self.stop_recording(None)?;
                } else {
                    self.start_recording();
                }
            }
            CameraOperation::RetryCamera => self.retry_camera(),
            CameraOperation::OpenCameraPrivacySettings => self.open_camera_privacy_settings(),
            CameraOperation::SelectCamera { device } => self.select_camera(&device),
            CameraOperation::ImportCodeImage => self.request_scan_image(),
            CameraOperation::CaptureDocumentPage => self.capture_document_page(),
            CameraOperation::RotateDocumentPage => self.rotate_document_page(),
            CameraOperation::RecognizeDocumentPage => self.recognize_document_page(),
            CameraOperation::CopyDocumentText => self.copy_document_text(),
            CameraOperation::ExportDocument => self.request_document_export(),
            CameraOperation::BeginDocumentCrop => self.begin_document_crop(),
            CameraOperation::MoveDocumentPageEarlier => self.move_document_page(false),
            CameraOperation::MoveDocumentPageLater => self.move_document_page(true),
            CameraOperation::RemoveDocumentPage => self.remove_document_page(),
        }
        Ok(())
    }

    /// Revalidate the real device list and force the live pipeline to reopen
    /// on its next sync. This does not claim a camera exists or request any
    /// permission itself.
    fn retry_camera(&mut self) {
        let devices = self.available_camera_devices();
        if let Some(selected) = &self.selected_camera
            && !devices.contains(selected)
        {
            self.selected_camera = None;
        }
        #[cfg(feature = "live-capture")]
        {
            self.camera = None;
            self.cam_config = None;
        }
        self.camera_issue = None;
        self.chooser.close();
        self.toast = Some(if devices.is_empty() {
            "No cameras found. Connect one, then refresh".into()
        } else {
            "Refreshing camera".into()
        });
    }

    fn open_camera_privacy_settings(&mut self) {
        // `settings://` is delegated to the desktop's registered settings
        // handler, just as scanned URLs use the OS handler. It is deliberately
        // an external effect, never a fabricated in-app permission grant.
        ui::widget::open_external_url("settings://privacy/camera");
        self.toast = Some("Opening camera privacy settings".into());
    }

    fn open_camera_chooser(&mut self) {
        let devices = self.available_camera_devices();
        if devices.len() > 1 {
            let selected = self
                .selected_camera
                .as_ref()
                .and_then(|selected| devices.iter().position(|device| device == selected));
            self.chooser.set_label("Camera");
            self.chooser.set_items(devices);
            self.chooser.set_viewport(self.rect);
            self.chooser.open_at(self.chooser_anchor, selected);
        } else {
            self.toast = Some("Only one camera is available".into());
        }
    }

    fn select_camera(&mut self, device: &str) {
        // Revalidate exactly when an agent, semantic action, or pointer asks
        // to select: a stale /dev/video node must not become a pretend lens.
        if !self.available_camera_devices().iter().any(|d| d == device) {
            self.chooser.close();
            self.toast = Some("That camera is no longer available".into());
            return;
        }
        self.selected_camera = Some(device.to_string());
        self.chooser.close();
        #[cfg(feature = "live-capture")]
        {
            self.camera = None;
            self.cam_config = None;
        }
        self.camera_issue = None;
        self.toast = Some(format!("Selected {device}"));
    }

    fn request_scan_image(&mut self) {
        if self.scan_image_picker.is_some() {
            return;
        }
        self.scan_image_picker = Some(
            ui::portal::PickRequest::open("Scan a code from an image", "Scan")
                .thread_name("camera-portal-scan-image")
                .filter(
                    ui::portal::FileFilter::new("Images")
                        .mimetype("image/png")
                        .mimetype("image/jpeg")
                        .mimetype("image/webp")
                        .mimetype("image/gif"),
                )
                .send(),
        );
        self.toast = Some("Choose an image containing a QR code or barcode".into());
    }

    fn poll_scan_image_picker(&mut self) {
        let Some(receiver) = self.scan_image_picker.as_ref() else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::portal::PortalOutcome::Failed("The file picker stopped responding".into())
            }
        };
        self.scan_image_picker = None;
        match outcome {
            ui::portal::PortalOutcome::Opened(path) => self.scan_image_path(path),
            ui::portal::PortalOutcome::Cancelled => {
                self.toast = Some("Image scan cancelled".into());
                if let Some(invocation) = self.pending_scan_import.take() {
                    self.completed_operations.insert(
                        invocation,
                        tilo_actions::InvocationUpdate::Cancelled {
                            invocation,
                            message: Some("image scan cancelled".into()),
                        },
                    );
                }
            }
            ui::portal::PortalOutcome::Failed(error) => {
                self.toast = Some(format!("Could not choose an image: {error}"));
                if let Some(invocation) = self.pending_scan_import.take() {
                    self.completed_operations.insert(
                        invocation,
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "camera.image_picker_failed".into(),
                                message: error,
                                details: None,
                                retryable: true,
                            },
                        },
                    );
                }
            }
        }
    }

    /// Decode a code from one portal-selected image. The path is ephemeral and
    /// never enters Camera's collaborative state or a private media index.
    fn scan_image_path(&mut self, path: std::path::PathBuf) {
        self.scan_dismissed = false;
        let Some(generation) = self.scan_generation.checked_add(1) else {
            self.toast = Some("Code scan counter exhausted; restart Camera".into());
            return;
        };
        let id = format!("portal:{generation}");
        if let Err(error) = self.worker.submit_scan(crate::worker::ScanJob {
            generation,
            id: id.clone(),
            source: crate::worker::ScanSource::Path(path),
            record_history: true,
        }) {
            self.toast = Some(error);
            return;
        }
        self.scan_generation = generation;
        self.pending_scan = Some((id, generation));
        self.toast = Some("Scanning image…".into());
    }

    /// Carry out the chosen action for a scanned code. Openable schemes are
    /// handed to the OS default handler; copies go to the clipboard.
    fn perform_scan_action(&mut self, s: &ScannedCode, i: usize) {
        let actions = s.kind.actions();
        let label = actions.get(i).copied().unwrap_or("Copy text");
        match (label, &s.kind) {
            ("Call", CodeKind::Contact { phone, .. }) => {
                ui::widget::open_external_url(&format!("tel:{phone}"));
                self.toast = Some(format!("Calling {phone}"));
            }
            ("Open link" | "Open map" | "Write email" | "Call", kind) => {
                ui::widget::open_external_url(&s.raw);
                self.toast = Some(format!("Opening {}", kind.summary(&s.raw)));
            }
            ("Copy password", CodeKind::Wifi { password, .. }) => {
                ui::clipboard_copy(password);
                self.toast = Some("Wi-Fi password copied".into());
            }
            ("Copy network name", CodeKind::Wifi { ssid, .. }) => {
                ui::clipboard_copy(ssid);
                self.toast = Some("Network name copied".into());
            }
            ("Copy link" | "Copy contact" | "Copy text", _) => {
                ui::clipboard_copy(&s.raw);
                self.toast = Some("Copied".into());
            }
            (_, kind) => {
                ui::clipboard_copy(&kind.summary(&s.raw));
                self.toast = Some("Copied".into());
            }
        }
    }

    fn hit(&mut self, pos: Point, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let found = self
            .regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(pos))
            .map(|(_, a)| a.clone());
        if let Some(action) = found {
            self.apply(action, doc, root);
            true
        } else {
            false
        }
    }

    #[cfg(feature = "live-capture")]
    fn poll_camera(&mut self) {
        // Only the live/disk storage drives a real capture device. The sample
        // storage backs `--shots`, the test suite and any headless render; it
        // synthesizes its viewfinder frame, so it must never enumerate or open
        // /dev/video*. The `live-capture` feature compiles the pipeline in; this
        // runtime check is what decides whether it actually runs — keeping the
        // hermetic path device-free even in a default (feature-on) build.
        if !matches!(self.storage, Storage::Disk(_)) {
            return;
        }

        // Pick a device for the selected lens: enumerate /dev/video* and index
        // by facing (rear/front/wide → 0/1/2), clamped to what's present.
        let devices = crate::capture::list_devices();
        let idx = match self.facing {
            Facing::Back => 0,
            Facing::Front => 1,
            Facing::Wide => 2,
        };
        let device = self
            .selected_camera
            .as_ref()
            .filter(|selected| devices.contains(selected))
            .cloned()
            .or_else(|| devices.get(idx).or_else(|| devices.first()).cloned());
        let (rw, rh) = capture_request_dims(self.settings.resolution);
        let want = device.map(|d| (d, rw, rh));

        // (Re)open when the desired device or resolution changes. Recording
        // holds the current pipeline so a clip isn't cut by a settings poke.
        if want != self.cam_config && !self.is_recording() {
            self.camera = None;
            if let Some((d, w, h)) = &want {
                // Try the requested size, then fall back to a broadly-supported
                // mode so the feed stays alive on modest webcams. Preserve a
                // meaningful failure category for the honest empty state.
                match crate::capture::Capture::open(d, *w, *h)
                    .or_else(|_| crate::capture::Capture::open(d, 1280, 720))
                {
                    Ok(camera) => {
                        self.camera = Some(camera);
                        self.camera_issue = None;
                    }
                    Err(error) => {
                        self.camera_issue = Some(camera_issue_for(error.downcast_ref()));
                    }
                }
            } else if std::env::var_os("CAMERA_SHOT_CAMERA_STATE").is_none() {
                self.camera_issue = None;
            }
            self.cam_config = want;
        }

        // A start the user refused, or a camera that went away, ends the
        // session; the settings key stays so a new request is not made every
        // frame.
        if let Some(error) = self
            .camera
            .as_mut()
            .and_then(crate::capture::Capture::failure)
        {
            self.camera = None;
            self.camera_issue = Some(camera_issue_for(Some(&error)));
        }
        if let Some(cam) = &mut self.camera
            && let Some(f) = cam.take_latest()
        {
            let _ = self.push_camera_frame(FrameBuf {
                rgba: f.rgba,
                w: f.width,
                h: f.height,
            });
        }
    }
}

fn cycle<T: Copy + PartialEq>(all: &[T], cur: T) -> T {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0);
    // Callers always pass a non-empty `ALL` array of enum variants; fall
    // back to `cur` rather than panicking if that ever stops holding.
    let next = (i + 1) % all.len().max(1);
    all.get(next).copied().unwrap_or(cur)
}

/// The empty-state category for a capture that could not start.
#[cfg(feature = "live-capture")]
/// Whether captures can be projected into a native directory here; a
/// browser page keeps them only in the content store.
fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

fn camera_issue_for(error: Option<&tilo_capture::CaptureError>) -> CameraIssue {
    match error {
        Some(tilo_capture::CaptureError::PermissionDenied) => CameraIssue::PermissionDenied,
        _ => CameraIssue::StartFailed,
    }
}

/// The frame size to request from a webcam for a chosen resolution preset.
/// Sensor-still presets (12 MP) exceed what a UVC video stream delivers, so
/// they're capped at a 4K video request; the pipeline negotiates the nearest
/// supported mode and the actual frame size is taken from the buffer.
#[cfg(feature = "live-capture")]
fn capture_request_dims(res: Resolution) -> (u32, u32) {
    match res {
        Resolution::R720p => (1280, 720),
        Resolution::R1080p => (1920, 1080),
        Resolution::R4K | Resolution::R12MP => (3840, 2160),
    }
}

/// Seconds since the Unix epoch on the wall clock a capture pins.
fn now_secs() -> i64 {
    ui::clock::local_now().timestamp()
}

fn fmt_dur(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    ui::format::timecode(u64::try_from(seconds).unwrap_or(0))
}

fn valid_frame(frame: &FrameBuf) -> bool {
    let Some(pixels) = u64::from(frame.w).checked_mul(u64::from(frame.h)) else {
        return false;
    };
    let Some(expected) = pixels.checked_mul(4).and_then(|n| usize::try_from(n).ok()) else {
        return false;
    };
    frame.w > 0
        && frame.h > 0
        && pixels <= MAX_FRAME_PIXELS
        && expected <= MAX_FRAME_BYTES
        && frame.rgba.len() == expected
}

fn frame_image_fill(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Fill)
}

fn frame_image_cover(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Cover)
}

/// Self-contained recent-capture fixture for the hermetic screenshot/test path.
/// Uses procedural scenes (`path = None`) so nothing is read from disk.
fn sample_recent_capture() -> Capture {
    let now = now_secs();
    Capture {
        id: "c1".into(),
        kind: CaptureKind::Photo,
        ts: now - 120,
        w: 4_032,
        h: 3_024,
        dur_ms: 0,
        label: "Golden hour".into(),
        facing: Facing::Back,
        scene: 1,
        path: None,
        // Screenshot fixtures need a stable portable identity so their
        // resource/search contracts exercise the same typed handoff as a
        // captured item. The headless fixture intentionally has no local
        // blob; the real launcher reports that absence honestly if invoked.
        media_id: Some(format!("{}{:064x}", crate::media::MEDIA_PREFIX, 1)),
    }
}

impl Widget for CameraApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::CAMERA_OPERATION_TYPE]
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
        if let Some(started) = self.admit_resource_operation_with_document(
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
        if cancellation_requested
            && self
                .pending_capture
                .is_some_and(|(pending, _)| pending == invocation)
        {
            self.pending_capture = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("photo save cancelled".into()),
            });
        }
        if cancellation_requested
            && self
                .pending_video
                .is_some_and(|(pending, _)| pending == invocation)
        {
            self.pending_video = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("video finalization cancelled".into()),
            });
        }
        if cancellation_requested && self.pending_scan_import == Some(invocation) {
            self.pending_scan_import = None;
            self.scan_image_picker = None;
            self.pending_scan = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("image scan cancelled".into()),
            });
        }
        if cancellation_requested && self.pending_document_job == Some(invocation) {
            self.pending_document_job = None;
            self.document_job = None;
            self.document_job_kind = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("document task cancelled".into()),
            });
        }
        if cancellation_requested && self.pending_document_export == Some(invocation) {
            self.pending_document_export = None;
            self.document_save_picker = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("document export cancelled".into()),
            });
        }
        self.completed_operations.remove(&invocation)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "sync is the one ordered document/lifecycle boundary"
    )]
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // The hosted controls read the values presented to them last frame.
        self.settings_sheet.dialog.sync(state, path);
        for field in &mut self.document_controls.fields {
            field.sync(state, path);
        }
        let (state, path) = ui::doc::document(state, path);
        self.poll_document_job();
        self.poll_document_export();
        self.poll_scan_image_picker();
        // An empty record already reads as Photo / Back / default settings.
        // Writing those defaults would be a phantom history entry.
        self.poll_countdown();
        while let Some(result) = self.worker.try_recv() {
            let terminal = result.invocation.filter(|invocation| {
                self.pending_capture.is_some_and(|(pending, generation)| {
                    pending == *invocation && generation == result.generation
                })
            });
            if terminal.is_some() {
                self.pending_capture = None;
            }
            // A cancelled/superseded agent invocation owns no durable result.
            // Local shutter jobs have no invocation and remain valid.
            if result.invocation.is_some() && terminal.is_none() {
                if let Ok(saved) = result.result
                    && let Some(path) = &saved.local_path
                {
                    crate::files::remove(path);
                }
                continue;
            }
            match result.result {
                Ok(saved) => {
                    let action = crate::actions::CameraAction::RegisterCapture {
                        id: saved.name.clone(),
                        media_id: saved.media_id.clone(),
                        kind: CaptureKind::Photo,
                        ts: now_secs(),
                        w: saved.w,
                        h: saved.h,
                        dur_ms: 0,
                        label: "New capture".into(),
                        facing: saved.facing,
                    };
                    if let Err(error) = self.try_perform_action(state, &action) {
                        if let Some(path) = &saved.local_path {
                            crate::files::remove(path);
                        }
                        self.toast = Some(format!("Save failed: {error}"));
                        if let Some(invocation) = terminal {
                            self.completed_operations.insert(
                                invocation,
                                tilo_actions::InvocationUpdate::Failed {
                                    invocation,
                                    error: tilo_actions::ActionFailure {
                                        code: "camera.register_refused".into(),
                                        message: error,
                                        details: None,
                                        retryable: false,
                                    },
                                },
                            );
                        }
                        continue;
                    }
                    let output = tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "media_id": saved.media_id.clone(),
                            "capture_id": saved.name.clone(),
                        })),
                        resources: vec![format!("camera.photo:{}", saved.name.clone())],
                    };
                    self.toast = Some("Photo saved".into());
                    if let Some(invocation) = terminal {
                        self.completed_operations.insert(
                            invocation,
                            tilo_actions::InvocationUpdate::Succeeded { invocation, output },
                        );
                    }
                }
                Err(error) => {
                    self.toast = Some(format!("Save failed: {error}"));
                    if let Some(invocation) = terminal {
                        self.completed_operations.insert(
                            invocation,
                            tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "camera.save_failed".into(),
                                    message: error,
                                    details: None,
                                    retryable: true,
                                },
                            },
                        );
                    }
                }
            }
        }
        while let Some(result) = self.worker.try_recv_video() {
            self.video_finalizing = false;
            let terminal = result.invocation.filter(|invocation| {
                self.pending_video.is_some_and(|(pending, generation)| {
                    pending == *invocation && generation == result.generation
                })
            });
            if terminal.is_some() {
                self.pending_video = None;
            }
            if result.invocation.is_some() && terminal.is_none() {
                if let Ok(saved) = result.result
                    && let Some(path) = saved.local_path
                {
                    crate::files::remove(&path);
                }
                continue;
            }
            match result.result {
                Ok(saved) => {
                    let action = crate::actions::CameraAction::RegisterCapture {
                        id: saved.name.clone(),
                        media_id: saved.media_id.clone(),
                        kind: CaptureKind::Video,
                        ts: now_secs(),
                        w: saved.w,
                        h: saved.h,
                        dur_ms: saved.dur_ms,
                        label: "New clip".into(),
                        facing: saved.facing,
                    };
                    if let Err(error) = self.try_perform_action(state, &action) {
                        if let Some(path) = &saved.local_path {
                            crate::files::remove(path);
                        }
                        self.toast = Some(format!("Save failed: {error}"));
                        if let Some(invocation) = terminal {
                            self.completed_operations.insert(
                                invocation,
                                tilo_actions::InvocationUpdate::Failed {
                                    invocation,
                                    error: tilo_actions::ActionFailure {
                                        code: "camera.register_refused".into(),
                                        message: error,
                                        details: None,
                                        retryable: false,
                                    },
                                },
                            );
                        }
                        continue;
                    }
                    if matches!(self.storage, Storage::Sample) {
                        self.captures.clear();
                        self.captures.push(Capture {
                            id: saved.name.clone(),
                            kind: CaptureKind::Video,
                            ts: now_secs(),
                            w: saved.w,
                            h: saved.h,
                            dur_ms: saved.dur_ms,
                            label: "New clip".into(),
                            facing: saved.facing,
                            scene: 0,
                            path: None,
                            media_id: Some(saved.media_id.clone()),
                        });
                    }
                    let output = tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "media_id": saved.media_id.clone(),
                            "capture_id": saved.name.clone(),
                        })),
                        resources: vec![format!("camera.video:{}", saved.name.clone())],
                    };
                    self.toast = Some("Video saved".into());
                    if let Some(invocation) = terminal {
                        self.completed_operations.insert(
                            invocation,
                            tilo_actions::InvocationUpdate::Succeeded { invocation, output },
                        );
                    }
                }
                Err(error) => {
                    self.toast = Some(format!("Save failed: {error}"));
                    if let Some(invocation) = terminal {
                        self.completed_operations.insert(
                            invocation,
                            tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "camera.video_failed".into(),
                                    message: error,
                                    details: None,
                                    retryable: true,
                                },
                            },
                        );
                    }
                }
            }
        }
        // A pending preview the pipeline no longer knows (dropped from a full
        // queue) simply leaves `thumb_pending`, so the next draw asks again.
        let settled: Vec<(String, Option<thumbnail::ThumbState>)> = self
            .thumb_pending
            .iter()
            .filter_map(|(id, key)| match self.thumb_service.peek(key) {
                Some(state) if state.is_loading() => None,
                state => Some((id.clone(), state)),
            })
            .collect();
        for (id, state) in settled {
            self.thumb_pending.remove(&id);
            match state {
                None => {}
                Some(thumbnail::ThumbState::Ready(preview)) => {
                    if self.thumbs.len() >= MAX_THUMB_CACHE {
                        self.thumbs.clear();
                    }
                    self.thumbs.insert(
                        id,
                        Image::from_rgba(
                            Arc::from(preview.rgba.as_slice()),
                            preview.width,
                            preview.height,
                        )
                        .with_fit(ImageFit::Cover),
                    );
                }
                _ => {
                    self.thumb_failed.insert(id);
                }
            }
            self.dirty = true;
        }
        while let Some(result) = self.worker.try_recv_scan() {
            if self.pending_scan.as_ref() != Some(&(result.id.clone(), result.generation)) {
                continue;
            }
            self.pending_scan = None;
            let mut terminal_error = None;
            match result.result {
                Ok(Some(scanned)) => {
                    self.scanned = Some(scanned.clone());
                    if result.record_history {
                        self.perform_action(
                            state,
                            &crate::actions::CameraAction::RecordScan {
                                raw: scanned.raw,
                                ts: now_secs(),
                            },
                        );
                        self.scans = doc::read_scans(state, path);
                    }
                    self.toast = Some(format!("{} found", scanned.kind.title()));
                }
                Ok(None) => {
                    self.scanned = None;
                    self.toast = Some("No code found in that photo".into());
                    terminal_error = Some((
                        "camera.code_not_found",
                        "No QR code or barcode was found in that image".to_owned(),
                        false,
                    ));
                }
                Err(error) => {
                    self.scanned = None;
                    self.toast = Some(format!("Scan failed: {error}"));
                    terminal_error = Some(("camera.scan_failed", error, true));
                }
            }
            if let Some(invocation) = self.pending_scan_import.take() {
                let update = terminal_error.map_or_else(
                    || tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                    |(code, message, retryable)| tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: code.into(),
                            message,
                            details: None,
                            retryable,
                        },
                    },
                );
                self.completed_operations.insert(invocation, update);
            }
            self.dirty = true;
        }
        // Dev/screenshot hook: force a screen without driving the nav.
        if let Ok(s) = std::env::var("CAMERA_SCREEN")
            && let Some(sc) = Screen::parse_str(&s)
        {
            self.screen = sc;
        }
        // Don't let a persisted mode flip mid-record.
        if self.recording.is_none() {
            self.mode = doc::read_mode(state, path);
            // Dev/screenshot hook (`--mode video`): pose the capture sub-mode
            // without driving the switcher. Same guard as the persisted mode;
            // an unrecognised value is ignored (like `CAMERA_SCREEN`) rather
            // than forcing Photo and making the switcher inert.
            if let Ok(mode) = std::env::var("CAMERA_MODE") {
                self.mode = match mode.as_str() {
                    "photo" => CaptureMode::Photo,
                    "video" => CaptureMode::Video,
                    _ => self.mode,
                };
            }
        }
        self.facing = doc::read_facing(state, path);
        self.settings = doc::read_settings(state, path);
        self.scans = doc::read_scans(state, path);

        // Screenshot-only presentation fixtures.  They are restricted to the
        // hermetic sample storage, so no launch option can arm a physical
        // camera or modify a collaborator's preferred capture settings.
        if matches!(self.storage, Storage::Sample) {
            if self.captures.is_empty() {
                self.captures = vec![sample_recent_capture()];
            }
            if let Ok(mode) = std::env::var("CAMERA_SHOT_MODE") {
                self.mode = CaptureMode::parse_str(&mode);
            }
            if std::env::var_os("CAMERA_SHOT_TIMER_RUNNING").is_some() {
                self.settings.timer = Timer::S3;
            }
            if std::env::var_os("CAMERA_SHOT_TIMER_RUNNING").is_some() && self.countdown.is_none() {
                self.countdown = Some(Countdown {
                    due: ui::time::now() + Duration::from_secs(3),
                    mode: self.mode,
                });
            }
        }

        // Offscreen fixtures may request a concrete unavailable state without
        // fabricating a feed. Production state below still comes from an
        // actual device-open result.
        let forced_camera_state = std::env::var("CAMERA_SHOT_CAMERA_STATE").ok();
        self.camera_issue = match forced_camera_state.as_deref() {
            Some("no-device") => Some(CameraIssue::NoDevice),
            Some("permission") => Some(CameraIssue::PermissionDenied),
            Some("error") => Some(CameraIssue::StartFailed),
            _ => self.camera_issue,
        };

        #[cfg(feature = "live-capture")]
        if forced_camera_state.is_none() {
            self.poll_camera();
        }

        // Hermetic path has no frame clock: while recording, synthesize a
        // procedural frame each tick so a real multi-frame clip accumulates.
        if self.recording.is_some() && matches!(self.storage, Storage::Sample) {
            let seed = Self::viewfinder_seed(self.facing).wrapping_add((self.frame_gen as u8) & 3);
            let fb = frame::viewfinder(seed, 480, 270);
            let _ = self.push_camera_frame(fb);
            self.dirty = true;
        }

        // Camera retains exactly one collaborative handoff. Photos and Video
        // Player own collection browsing, deletion, favorites, and metadata.
        if matches!(self.storage, Storage::Disk(_)) {
            let next = doc::read_recent_capture(state, path).map(|shared| Capture {
                id: shared.id,
                kind: shared.kind,
                ts: shared.ts,
                w: shared.w,
                h: shared.h,
                dur_ms: shared.dur_ms,
                label: shared.label,
                facing: shared.facing,
                scene: 0,
                path: None,
                media_id: Some(shared.media_id),
            });
            let changed = self.captures.first().map(|capture| &capture.id)
                != next.as_ref().map(|capture| &capture.id);
            self.captures = next.into_iter().collect();
            if changed {
                self.thumb_pending.clear();
                self.thumb_failed.clear();
                self.thumbs.clear();
                self.dirty = true;
            }
        }
        self.refresh_search_provider();
        if self.screen == Screen::Document
            && self.document_scan.pages.is_empty()
            && std::env::var_os("CAMERA_SCAN_FIRST").is_some()
        {
            self.capture_document_page();
        }
        if self.screen == Screen::Document
            && !self.document_scan.pages.is_empty()
            && self.document_crop_editor.is_none()
            && std::env::var_os("CAMERA_CROP").is_some()
        {
            self.begin_document_crop();
        }
        // A deterministic cursor fixture makes it possible to inspect compact
        // control tooltips without synthesizing a pointer event or reaching a
        // real device.  It is deliberately presentation-only and ignored if
        // malformed, just like the other `--shots` knobs.
        if let Ok(v) = std::env::var("CAMERA_SHOT_HOVER")
            && let Some((x, y)) = v.split_once(',')
            && let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>())
            && x.is_finite()
            && y.is_finite()
        {
            self.last_mouse = Point::new(x, y);
        }
        self.present_controls();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        self.built_with_document = self.base_screen() == Screen::Document;
        self.document_controls.nodes.clear();
        if self.built_with_document {
            for field in &mut self.document_controls.fields {
                let node = field.build_layout(taffy, theme, node_map);
                self.document_controls.nodes.push(node);
            }
        }
        // The hosted dropdowns are children so they join focus order and the
        // semantic tree; the document view places them itself.
        #[allow(clippy::unwrap_used, reason = "adding a node to the tree cannot fail")]
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &self.document_controls.nodes,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if self.built_with_document {
            let placed = Self::document_layout(self.rect, &self.layout_theme);
            for (field, rect) in self.document_controls.fields.iter_mut().zip(placed.fields) {
                field.apply_layout_external(rect);
            }
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = (self.base_screen() == Screen::Document) != self.built_with_document;
        for field in &mut self.document_controls.fields {
            dirty |= field.take_structure_dirty();
        }
        dirty
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.regions.clear();
        self.tooltip_targets.clear();
        // Camera is an immersive capture surface: the viewfinder fills the
        // window and the settings sheet floats over it.
        let full = self.rect;
        let m = hud::HudMetrics::of(theme);
        // A toast rises above the bottom controls of the screen it reports on.
        let toast_area = match self.base_screen() {
            Screen::Camera | Screen::Settings => {
                self.draw_camera(scene, t, tr, theme, full);
                let reserved = 2.0 * m.edge
                    + m.bar()
                    + f64::from(theme.tokens.spacing.s4)
                    + 2.0 * f64::from(theme.tokens.metrics.min_pointer_target);
                Rect::new(full.x0, full.y0, full.x1, full.y1 - reserved)
            }
            Screen::Scanner => {
                self.draw_scanner(scene, t, tr, theme, full);
                Rect::new(full.x0, full.y0, full.x1, full.y1 - 2.0 * m.edge - m.bar())
            }
            Screen::Document => {
                self.draw_document_scanner(scene, t, tr, theme, presence, full);
                let page = Self::document_layout(full, theme).page;
                let actions = ui::widget::ControlSize::Regular.height(theme)
                    + f64::from(theme.tokens.spacing.s4);
                Rect::new(page.x0, full.y0, page.x1, page.y1 - actions)
            }
        };
        if self.screen == Screen::Settings {
            // Under the modal sheet only the sheet is operable.
            self.regions.clear();
            self.tooltip_targets.clear();
        } else {
            self.draw_tooltip(scene, t, tr, theme);
        }
        self.expire_toast();
        self.draw_toast(scene, t, tr, theme, toast_area);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the root event router keeps the modal sheet, the hosted controls, pointer, touch and keyboard precedence together so painted and semantic hit-testing cannot diverge"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use winit::event::{ElementState, KeyEvent, MouseButton, TouchPhase};
        use winit::keyboard::{Key, NamedKey};

        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.dirty = true;
        }
        let path = ctx.path;
        // The settings sheet is modal: it takes every event while it is up.
        if self.screen == Screen::Settings {
            self.settings_sheet.dialog.handle_detached(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                path,
            );
            *ctx.handled = true;
            self.drain_ui_queue(ctx.state, path);
            self.dirty = true;
            return;
        }
        if self.chooser.is_open() {
            self.chooser.handle_event(event, ctx);
            if let Some(index) = self.chooser.take_selection()
                && let Some(device) = self.chooser.items().get(index).cloned()
            {
                self.apply(Action::SelectCamera(device), ctx.state, path);
            }
            *ctx.handled = true;
            self.dirty = true;
            return;
        }
        if self.built_with_document {
            // An open list takes every event until it closes.
            let open = self
                .document_controls
                .fields
                .iter()
                .position(ui::widget::Dropdown::is_open);
            let count = self.document_controls.fields.len();
            let order: Vec<usize> = open.map_or_else(|| (0..count).collect(), |i| vec![i]);
            for i in order {
                let node = self
                    .document_controls
                    .nodes
                    .get(i)
                    .copied()
                    .unwrap_or(ctx.node_id);
                if let Some(field) = self.document_controls.fields.get_mut(i) {
                    field.handle_event(event, &mut ctx.reborrow(path, node));
                }
                if *ctx.handled {
                    break;
                }
            }
            self.drain_ui_queue(ctx.state, path);
            if open.is_some() || *ctx.handled {
                self.dirty = true;
                return;
            }
        }
        if let Some(editor) = self.document_crop_editor.as_mut()
            && editor.handle_event_at(event, ctx.scale_factor)
        {
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.hit(self.last_mouse, ctx.state, path) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                match touch.phase {
                    TouchPhase::Started if self.active_touch.is_none() => {
                        if let Some((rect, action)) = self
                            .regions
                            .iter()
                            .rev()
                            .find(|(rect, _)| rect.contains(point))
                            .cloned()
                        {
                            self.active_touch = Some(TouchTarget {
                                id: touch.id,
                                rect,
                                action,
                                cancelled: false,
                            });
                            *ctx.handled = true;
                        }
                    }
                    TouchPhase::Moved => {
                        if let Some(target) = self.active_touch.as_mut()
                            && target.id == touch.id
                        {
                            if !target.rect.contains(point) {
                                target.cancelled = true;
                            }
                            *ctx.handled = true;
                        }
                    }
                    TouchPhase::Ended => {
                        if self
                            .active_touch
                            .as_ref()
                            .is_some_and(|target| target.id == touch.id)
                        {
                            let target = self.active_touch.take();
                            if let Some(target) = target
                                && !target.cancelled
                                && target.rect.contains(point)
                            {
                                self.apply(target.action, ctx.state, path);
                            }
                            *ctx.handled = true;
                        }
                    }
                    TouchPhase::Cancelled
                        if self
                            .active_touch
                            .as_ref()
                            .is_some_and(|target| target.id == touch.id) =>
                    {
                        self.active_touch = None;
                        *ctx.handled = true;
                    }
                    TouchPhase::Cancelled | TouchPhase::Started => {}
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let act = match logical_key {
                    Key::Character(c) => match c.as_str() {
                        "1" => Some(Action::Nav(Screen::Camera)),
                        "2" => Some(Action::Nav(Screen::Scanner)),
                        "3" | "d" | "D" => Some(Action::Nav(Screen::Document)),
                        "4" => Some(Action::Nav(Screen::Settings)),
                        "p" | "P" => Some(Action::OpenPhotos),
                        "g" | "G" => Some(Action::ToggleGrid),
                        "f" | "F" => Some(Action::CycleFacing),
                        "r" | "R"
                            if matches!(self.screen, Screen::Camera) && !self.feed_active() =>
                        {
                            Some(Action::RetryCamera)
                        }
                        " " if matches!(self.screen, Screen::Camera) => Some(Action::Shutter),
                        _ => None,
                    },
                    Key::Named(NamedKey::Space) if matches!(self.screen, Screen::Camera) => {
                        Some(Action::Shutter)
                    }
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Scanner) => {
                        Some(Action::DismissScan)
                    }
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Document) => {
                        Some(if self.document_crop_editor.is_some() {
                            Action::CancelDocumentCrop
                        } else {
                            Action::Nav(Screen::Camera)
                        })
                    }
                    _ => None,
                };
                if let Some(a) = act {
                    self.apply(a, ctx.state, path);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.built_with_document {
            for field in &self.document_controls.fields {
                children.push(field);
            }
        }
        if self.settings_sheet.dialog.is_open() {
            children.push(&self.settings_sheet.dialog);
        }
        if self.chooser.is_open() {
            children.push(&self.chooser);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.built_with_document {
            for field in &self.document_controls.fields {
                visitor(field);
            }
        }
        if self.settings_sheet.dialog.is_open() {
            visitor(&self.settings_sheet.dialog);
        }
        if self.chooser.is_open() {
            visitor(&self.chooser);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if self.built_with_document {
            for field in &mut self.document_controls.fields {
                children.push(field);
            }
        }
        if self.settings_sheet.dialog.is_open() {
            children.push(&mut self.settings_sheet.dialog);
        }
        if self.chooser.is_open() {
            children.push(&mut self.chooser);
        }
        children
    }

    fn name(&self) -> &'static str {
        "CameraApp"
    }
    fn test_id(&self) -> Option<&str> {
        Some("camera_app")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_idle_sync(&self) -> bool {
        self.document_save_picker.is_some()
            || self.scan_image_picker.is_some()
            || self.document_job.is_some()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        // `regions` is rebuilt by `draw` from the actual screen and content.
        // Deduplicate identical actions: a backdrop and a visible back affordance
        // can share one action, but must not publish colliding semantic ids.
        let mut targets = HashSet::new();
        let mut controls: Vec<AccessNode> = self
            .regions
            .iter()
            .filter_map(|(rect, action)| {
                let target = Self::a11y_target(action);
                targets
                    .insert(target)
                    .then(|| self.a11y_node(*rect, action))
            })
            .collect();
        if let Some(editor) = self.document_crop_editor.as_ref()
            && let Some(crop) = editor.accessibility()
        {
            controls.push(crop);
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name("Camera")
                .with_bounds(self.rect)
                .with_children(controls),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if let Some(editor) = self.document_crop_editor.as_mut()
            && editor.perform_semantic_action(target, action, state, path)
        {
            return true;
        }
        if !matches!(action, A11yAction::Activate | A11yAction::ToggleCheck) {
            return false;
        }
        let hit = self
            .regions
            .iter()
            .find(|(_, candidate)| Self::a11y_target(candidate) == target)
            .map(|(_, candidate)| candidate.clone());
        if let Some(hit) = hit {
            self.apply(hit, state, path);
            true
        } else {
            false
        }
    }
    fn needs_redraw(&self) -> bool {
        let mut dirty = self.dirty || self.tooltip.needs_frame();
        self.for_each_child(&mut |child| dirty |= child.needs_redraw());
        dirty
    }
    fn needs_animation_frame(&self) -> bool {
        // Recording is an operational frame pump, not a decorative effect:
        // it advances the elapsed indicator and drains the real camera worker.
        // Reduced-motion users receive the same capture correctness through a
        // low-frequency wake deadline below, without a continuous UI loop.
        // A live viewfinder is content, not motion: it runs regardless.
        let mut animating = self.viewfinder_live()
            || self.tooltip.needs_frame()
            || ((self.recording.is_some() || self.countdown.is_some()) && !self.motion_reduced);
        self.for_each_child(&mut |child| animating |= child.needs_animation_frame());
        animating
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.awaiting_camera() {
            // Where access is prompted (the page, macOS) devices appear
            // asynchronously; look again rather than wait for input.
            return Some(ui::time::now() + Duration::from_secs(1));
        }
        // A toast goes away by itself once its time is up.
        let toast = self
            .toast_shown
            .as_ref()
            .map(|(_, since)| *since + ui::widget::toast::DEFAULT_TTL);
        if self.motion_reduced && (self.recording.is_some() || self.countdown.is_some()) {
            // The visible counter changes once per second; four wakeups per
            // second keep worker settlement responsive without recreating a
            // 30 fps decorative frame loop for reduced-motion users.
            let counter = ui::time::now() + Duration::from_millis(250);
            Some(toast.map_or(counter, |toast| toast.min(counter)))
        } else {
            toast
        }
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        self.dirty = true;
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        for child in self.children_mut() {
            child.mark_drawn();
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui::permissions::PermissionContext,
    ) {
        self.draw(scene, t, tr, theme, presence);
    }
}

impl ui::search::capability::SearchSource for CameraApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for CameraApp {
    /// Publish capture resources so the host's Ctrl+K action palette
    /// scopes to them. When the framework hosts the surface,
    /// [`tilo_resource::publish`] already owns Document prefs / mode / lens;
    /// only live Effect extras (shutter, devices, scanners) remain here.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == crate::resource::CAMERA_RESOURCE_ID {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::widget::controls::ActionControls for CameraApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.photos_browse
            .as_mut()
            .and_then(ActionEmitter::take)
            .or_else(|| self.photos_handoff.as_mut().and_then(ActionEmitter::take))
            .or_else(|| self.video_handoff.as_mut().and_then(ActionEmitter::take))
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        // The visible recent-capture tile is one control whose canonical
        // cross-app destination depends on the current capture type. Preserve
        // that one semantic identity while attaching the same emitter the
        // pointer path uses; do not inventory two invisible destination
        // controls.
        let recent_handoff = self.captures.first().and_then(|capture| {
            if capture.is_video() {
                self.video_handoff.as_ref()
            } else {
                self.photos_handoff.as_ref()
            }
        });
        if let Some(handoff) = recent_handoff
            && let Some(binding) = bindings
                .iter_mut()
                .find(|binding| binding.control_id == "camera.open-recent-capture")
        {
            *binding = handoff.metadata("camera.open-recent-capture");
        }
        if let Some(handoff) = self.photos_browse.as_ref()
            && let Some(binding) = bindings
                .iter_mut()
                .find(|binding| binding.control_id == "camera.open-photos")
        {
            *binding = handoff.metadata("camera.open-photos");
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(handoff) = self.photos_handoff.as_mut() {
            handoff.refresh_availability(allowed);
        }
        if let Some(handoff) = self.photos_browse.as_mut() {
            handoff.refresh_availability(allowed);
        }
        if let Some(handoff) = self.video_handoff.as_mut() {
            handoff.refresh_availability(allowed);
        }
    }
}

impl ui::undo::CommittedActions for CameraApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its metadata action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

impl CameraApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        self.perform_resource_operation(operation)?;
        Ok(true)
    }

    fn admit_resource_operation_with_document(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &mut AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        match operation {
            crate::actions::CameraOperation::Capture => {
                self.capture(Some(invocation))?;
                if matches!(self.storage, Storage::Disk(_)) {
                    return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
                }
            }
            crate::actions::CameraOperation::ToggleRecording if self.is_recording() => {
                self.stop_recording(Some(invocation))?;
                // Both live and hermetic paths finalize through the bounded
                // worker, so neither may report completion before the clip is
                // muxed, verified, and canonically registered.
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            crate::actions::CameraOperation::ImportCodeImage => {
                self.request_scan_image();
                if self.scan_image_picker.is_none() {
                    return Err("The image picker could not be opened".into());
                }
                self.pending_scan_import = Some(invocation);
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            crate::actions::CameraOperation::CaptureDocumentPage => {
                let pages_before = self.document_scan.pages.len();
                self.capture_document_page();
                if self.document_job.is_some() {
                    self.pending_document_job = Some(invocation);
                    return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
                }
                if self.document_scan.pages.len() == pages_before {
                    return Err("No document scanner is available".into());
                }
            }
            crate::actions::CameraOperation::RecognizeDocumentPage => {
                self.recognize_document_page();
                if self.document_job.is_none() {
                    return Err("Text recognition could not start".into());
                }
                self.pending_document_job = Some(invocation);
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            crate::actions::CameraOperation::ExportDocument => {
                self.request_document_export();
                if self.document_save_picker.is_none() {
                    return Err("Document export could not start".into());
                }
                self.pending_document_export = Some(invocation);
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            operation => self.perform_resource_operation(operation)?,
        }
        Ok(Some(ui::widget::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// Test-only accessors that let sibling modules (`crate::actions` tests) drive
/// the *real* affordance path and observe its result, without exposing internal
/// fields to the rest of the crate. Each helper routes through the same
/// production code an on-screen control does.
#[cfg(test)]
impl CameraApp {
    /// The `EncodedAction` the most recent affordance dispatched.
    pub(crate) fn test_last_dispatched(&self) -> Option<EncodedAction> {
        self.actions.last_dispatched().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::classify;
    use ui::headless::{Framed, viewport};

    fn sample_app() -> (CameraApp, AutoCommit, ObjId) {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc, root)
    }

    /// A fresh camera record stays empty. Readers still report Photo, the back
    /// camera, and default settings.
    #[test]
    fn a_fresh_camera_record_stays_empty_until_a_capture_action() {
        let (_app, doc, root) = sample_app();
        assert!(
            !doc::is_seeded(&doc, &root),
            "default preferences are not written into a new record"
        );
        assert_eq!(doc::read_mode(&doc, &root), CaptureMode::Photo);
        assert_eq!(doc::read_facing(&doc, &root), Facing::Back);
        assert_eq!(doc::read_settings(&doc, &root), Settings::default());
    }

    fn sync_until(
        app: &mut CameraApp,
        doc: &mut AutoCommit,
        root: &ObjId,
        mut done: impl FnMut(&CameraApp) -> bool,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(app) && ui::time::now() < deadline {
            app.sync(doc, root);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            done(app),
            "bounded camera worker did not settle in time: toast={:?}, video_finalizing={}, captures={:?}",
            app.toast,
            app.video_finalizing,
            app.captures
                .iter()
                .map(|capture| capture.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// Mount `app` over the case's `doc` in the production frame host and run
    /// one full frame (sync → layout → draw) at a given viewport. CPU-only; no
    /// GPU needed (rasterization is skipped — we only exercise layout + scene
    /// build).
    fn render_at(app: CameraApp, doc: AutoCommit, w: f32, h: f32) -> Framed<CameraApp> {
        render_with_theme(app, doc, w, h, Theme::dark())
    }

    fn render_at_scaled(
        app: CameraApp,
        doc: AutoCommit,
        w: f32,
        h: f32,
        text_scale: f32,
    ) -> Framed<CameraApp> {
        render_with_theme(app, doc, w, h, Theme::dark().with_text_scale(text_scale))
    }

    fn render_with_theme(
        app: CameraApp,
        doc: AutoCommit,
        w: f32,
        h: f32,
        theme: Theme,
    ) -> Framed<CameraApp> {
        let mut state = ui::State::new();
        assert!(
            state.adopt_doc(doc),
            "the harness adopts the case's document"
        );
        Framed::new(state, app, viewport((w, h)), theme)
    }

    /// Rebuild and run the next frame of an already-rendered app at `w`x`h`.
    fn rerender_at(app: &mut Framed<CameraApp>, w: f32, h: f32) {
        Framed::harness_mut(app)
            .relayout_themed(viewport((w, h)), Theme::dark())
            .expect("camera frame lays out");
    }

    fn drive(app: &mut CameraApp, ev: &WindowEvent, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let mods = winit::event::Modifiers::default();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: doc,
                path: root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    /// Every control the camera exposes, by semantic target.
    fn control_bounds(app: &CameraApp) -> Vec<(String, Rect)> {
        app.accessibility()
            .expect("camera has a semantic window")
            .children
            .iter()
            .filter_map(|node| {
                Some((
                    node.action_target.clone()?,
                    node.bounds.map(ui::a11y::AccessBounds::to_rect)?,
                ))
            })
            .collect()
    }

    fn bounds_of(controls: &[(String, Rect)], target: &str) -> Rect {
        controls
            .iter()
            .find(|(name, _)| name == target)
            .map_or_else(|| panic!("{target} is a control"), |(_, rect)| *rect)
    }

    #[test]
    fn viewfinder_controls_stay_inside_the_window_and_apart_at_every_size() {
        // Phase `controls_fit_and_never_overlap`.
        for (w, h) in [(1366.0, 768.0), (800.0, 600.0), (390.0, 844.0)] {
            for scale in [1.0_f32, 2.0] {
                let (app, doc, _root) = sample_app();
                let app = render_at_scaled(app, doc, w, h, scale);
                let window = app.rect;
                let controls = control_bounds(&app);
                for (target, rect) in &controls {
                    assert!(
                        rect.x0 >= window.x0 - 0.5
                            && rect.x1 <= window.x1 + 0.5
                            && rect.y0 >= window.y0 - 0.5
                            && rect.y1 <= window.y1 + 0.5,
                        "{w}x{h} at {scale}×: {target} {rect:?} leaves the window"
                    );
                }
                for (i, (a, ra)) in controls.iter().enumerate() {
                    for (b, rb) in controls.iter().skip(i + 1) {
                        assert!(
                            ra.intersect(*rb).area() < 1.0,
                            "{w}x{h} at {scale}×: {a} {ra:?} overlaps {b} {rb:?}"
                        );
                    }
                }
                let shutter = bounds_of(&controls, "camera.shutter");
                let photo = bounds_of(&controls, "camera.mode:photo");
                assert!(
                    photo.y0 - shutter.y1 >= f64::from(Theme::dark().tokens.spacing.s4) - 0.5,
                    "{w}x{h} at {scale}×: the switcher {photo:?} keeps a group gap under the shutter {shutter:?}"
                );
            }
        }
        // Phase `mode_switcher_segments_are_equal_and_in_order`.
        {
            let (app, doc, _root) = sample_app();
            let app = render_at(app, doc, 1280.0, 800.0);
            let controls = control_bounds(&app);
            let segments: Vec<Rect> = [
                "camera.mode:photo",
                "camera.mode:video",
                "camera.nav:scanner",
                "camera.nav:document",
            ]
            .iter()
            .map(|target| bounds_of(&controls, target))
            .collect();
            for pair in segments.windows(2) {
                assert!(
                    (pair[0].x1 - pair[1].x0).abs() < 0.5,
                    "{pair:?} are adjacent"
                );
                assert!((pair[0].width() - pair[1].width()).abs() < 0.5);
            }
        }
        // Phase `viewfinder_layout_remains_local`.
        {
            let (app, mut doc, _root) = sample_app();
            let heads = doc.get_heads();
            let mut app = render_at(app, doc, 1_100.0, 720.0);
            rerender_at(&mut app, 390.0, 720.0);
            assert!((app.rect.width() - 390.0).abs() < 0.5);
            assert_eq!(
                Framed::split(&mut app).1.doc.get_heads(),
                heads,
                "viewfinder layout remains local"
            );
        }
        // Phase `resize_reflows_the_same_instance`.
        {
            // One instance rendered wide then narrow reflows its scan feed
            // rather than keeping the first frame's layout.
            let app = CameraApp::with(Storage::Sample, Screen::Scanner);
            let doc = AutoCommit::new();
            let mut app = render_at(app, doc, 1280.0, 720.0);
            assert_eq!(app.screen, Screen::Scanner);
            let wide_w = app.scan_view.as_ref().expect("wide viewfinder").0.0;
            rerender_at(&mut app, 560.0, 720.0);
            let narrow_w = app.scan_view.as_ref().expect("narrow viewfinder").0.0;
            assert!(
                narrow_w < wide_w,
                "viewfinder reflowed {wide_w} -> {narrow_w}"
            );
        }
    }

    #[test]
    fn the_scan_result_never_covers_the_scan_guide() {
        for (w, h) in [(1280.0, 800.0), (800.0, 600.0), (390.0, 844.0)] {
            let app = CameraApp::with(Storage::Sample, Screen::Scanner);
            let doc = AutoCommit::new();
            let app = render_at(app, doc, w, h);
            let code = app.scanned.clone().expect("the sample poster decodes");
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let layout = CameraApp::scanner_layout(&mut tr, &theme, app.rect, Some(&code));
            let card = layout.card.expect("a decoded code has a card");
            assert!(
                card.intersect(layout.guide).area() <= 0.0,
                "{w}x{h}: card {card:?} covers the guide {:?}",
                layout.guide
            );
            let (gw, gh) = app.scan_gen;
            let sx = app.rect.width() / f64::from(gw);
            let sy = app.rect.height() / f64::from(gh);
            for (x, y) in code.corners {
                let corner = Point::new(f64::from(x) * sx, f64::from(y) * sy);
                assert!(
                    layout.guide.inflate(1.0, 1.0).contains(corner),
                    "{w}x{h}: the sample code {corner:?} sits inside the guide {:?}",
                    layout.guide
                );
            }
        }
    }

    fn escape_key() -> winit::event::KeyEvent {
        use winit::keyboard::{Key, KeyLocation, NamedKey, NativeKeyCode, PhysicalKey};
        // SAFETY: the zeroed fields that own data are overwritten without
        // being dropped; the dialog reads only `logical_key` and `state`.
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        unsafe {
            std::ptr::addr_of_mut!(event.logical_key).write(Key::Named(NamedKey::Escape));
            std::ptr::addr_of_mut!(event.physical_key)
                .write(PhysicalKey::Unidentified(NativeKeyCode::Unidentified));
            std::ptr::addr_of_mut!(event.text).write(None);
        }
        event.location = KeyLocation::Standard;
        event.state = winit::event::ElementState::Pressed;
        event.repeat = false;
        event
    }

    #[test]
    fn the_settings_sheet_opens_over_the_viewfinder_and_applies_changes() {
        // Phase `sheet_is_a_modal_over_the_viewfinder`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::Nav(Screen::Settings), &mut doc, &root);
            assert!(app.settings_sheet.dialog.is_open());
            assert_eq!(app.base_screen(), Screen::Camera);
            let mut app = render_at(app, doc, 1280.0, 800.0);
            assert!(
                control_bounds(&app).is_empty(),
                "the viewfinder controls are inert under the sheet"
            );
            let escape = WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: escape_key(),
                is_synthetic: false,
            };
            let (camera, state) = Framed::split(&mut app);
            assert!(drive(camera, &escape, &mut state.doc, &root));
            assert_eq!(camera.screen, Screen::Camera, "Escape dismisses the sheet");
            assert!(!camera.settings_sheet.dialog.is_open());
        }
        // Phase `sheet_controls_apply_through_camera_actions`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::Nav(Screen::Settings), &mut doc, &root);
            for action in [
                Action::SetResolution(Resolution::R4K),
                Action::SetPhotoFormat(PhotoFormat::Png),
                Action::SetTimer(Timer::S10),
                Action::ToggleHdr,
                Action::SetDefaultFacing(Facing::Front),
            ] {
                app.ui_queue.borrow_mut().push(action);
            }
            app.drain_ui_queue(&mut doc, &root);
            let settings = doc::read_settings(&doc, &root);
            assert_eq!(settings.resolution, Resolution::R4K);
            assert_eq!(settings.photo_format, PhotoFormat::Png);
            assert_eq!(settings.timer, Timer::S10);
            assert_eq!(settings.hdr, !Settings::default().hdr);
            assert_eq!(settings.default_facing, Facing::Front);
            assert_eq!(app.screen, Screen::Settings, "changes keep the sheet open");
        }
    }

    #[test]
    fn semantic_controls_are_plainly_named_and_dispatch_camera_actions() {
        // Phase `detected_label_has_single_space`.
        {
            let tag = scanner_view::detected_label("QR Code");
            assert_eq!(tag, "QR code detected");
            assert!(
                !tag.contains("  "),
                "detected label must not contain a double space: {tag:?}"
            );
        }
        // Phase `lens_label_never_reads_as_navigation`.
        {
            // The lens selector caption must not be the word "Back".
            assert_eq!(Facing::Back.label(), "Rear");
            assert_ne!(Facing::Back.label(), "Back");
        }
        // Phase `accessibility_exports_named_controls_from_the_painted_hit_map`.
        {
            let (app, doc, _root) = sample_app();
            let app = render_at(app, doc, 900.0, 700.0);
            let tree = app.accessibility().expect("camera has a semantic window");
            assert_eq!(tree.role, Role::Window);
            assert_eq!(tree.name.as_deref(), Some("Camera"));
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Take photo")
                    && node.role == Role::Button
                    && node.action_target.as_deref() == Some("camera.shutter")
            }));
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Open latest capture in Photos")
                    && node.role == Role::Button
                    && node.action_target.as_deref() == Some("camera.open-recent-capture")
            }));
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Open Photos to browse your library")
                    && node.role == Role::Button
                    && node.action_target.as_deref() == Some("camera.open-photos")
            }));
            assert!(tree.children.iter().all(|node| node.bounds.is_some()));
        }
        // Phase `capture_switcher_exposes_document_capture_without_a_gallery_surface`.
        {
            let (app, doc, _root) = sample_app();
            let app = render_at(app, doc, 1_200.0, 760.0);
            let tree = app
                .accessibility()
                .expect("camera has semantic capture controls");
            assert!(tree.children.iter().any(|node| {
                node.name.as_deref() == Some("Open Documents")
                    && node.action_target.as_deref() == Some("camera.nav:document")
            }));
            assert!(tree.children.iter().all(|node| {
                !node
                    .action_target
                    .as_deref()
                    .is_some_and(|target| target.contains("gallery") || target.contains("library"))
            }));
        }
        // Phase `browse_photos_uses_the_typed_owner_handoff_without_a_gallery`.
        {
            let (app, doc, root) = sample_app();
            let mut app = render_at(app, doc, 900.0, 700.0);
            let (camera, state) = Framed::split(&mut app);
            assert!(camera.perform_semantic_action(
                "camera.open-photos",
                &A11yAction::Activate,
                &mut state.doc,
                &root,
            ));
            let invocation = ui::widget::controls::take_control_invocation(&mut *app)
                .expect("Photos browse emits a typed external invocation");
            assert_eq!(invocation.address.resource, "camera.capture");
            assert_eq!(invocation.address.action, "open-photos");
            assert!(invocation.arguments.is_empty());
            assert!(
                app.captures.len() <= 1,
                "Camera still owns no gallery cache"
            );
        }
        // Phase `accessibility_activation_reuses_camera_action_dispatch`.
        {
            let (app, doc, root) = sample_app();
            let mut app = render_at(app, doc, 900.0, 700.0);
            let before = app.captures[0].id.clone();
            let (camera, state) = Framed::split(&mut app);
            assert!(camera.perform_semantic_action(
                "camera.shutter",
                &A11yAction::Activate,
                &mut state.doc,
                &root,
            ));
            assert_eq!(app.captures.len(), 1);
            assert_ne!(app.captures[0].id, before);
        }
    }

    #[test]
    fn unavailable_camera_recovery_is_honest_semantic_and_fits_touch_rows() {
        // Phase `permission_recovery_is_one_semantic_action`.
        {
            let dir = std::env::temp_dir().join("camera-recovery-test-no-device");
            let mut app = CameraApp::with(Storage::Disk(dir), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.camera_issue = Some(CameraIssue::PermissionDenied);
            let app = render_at(app, doc, 320.0, 680.0);
            let tree = app.accessibility().expect("camera has semantics");
            let privacy = tree
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Open camera privacy settings"))
                .expect("privacy recovery action");
            let bounds = privacy.bounds.expect("recovery has bounds");
            let target = f64::from(Theme::dark().tokens.metrics.min_pointer_target);
            assert!(bounds.x1 - bounds.x0 >= target && bounds.y1 - bounds.y0 >= target - 0.5);
        }
        // Phase `refresh_reports_the_discovered_state`.
        {
            let dir = std::env::temp_dir().join("camera-recovery-test-refresh");
            let mut app = CameraApp::with(Storage::Disk(dir), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.camera_issue = Some(CameraIssue::NoDevice);
            let mut app = render_at(app, doc, 390.0, 844.0);
            let (camera, state) = Framed::split(&mut app);
            assert!(camera.perform_semantic_action(
                "camera.retry-camera",
                &A11yAction::Activate,
                &mut state.doc,
                &root,
            ));
            // Device discovery is deliberately real: a test host may expose a
            // `/dev/video*` node, so assert the honest result for what it found.
            let expected = if app.available_camera_devices().is_empty() {
                "No cameras found. Connect one, then refresh"
            } else {
                "Refreshing camera"
            };
            assert_eq!(app.toast.as_deref(), Some(expected));
        }
        // Phase `sample_fixture_can_render_the_same_honest_permission_recovery`.
        {
            // The screenshot harness uses `Storage::Sample` to avoid both a real
            // camera and private imagery. An explicit permission result still
            // wins over that procedural feed.
            let (mut app, doc, _root) = sample_app();
            app.camera_issue = Some(CameraIssue::PermissionDenied);
            assert!(!app.feed_active());
            let app = render_at(app, doc, 390.0, 844.0);
            let tree = app.accessibility().expect("camera has semantics");
            assert!(
                tree.children
                    .iter()
                    .any(|node| { node.name.as_deref() == Some("Open camera privacy settings") })
            );
        }
        // Phase `sample_fixture_can_render_the_honest_no_device_state`.
        {
            let (mut app, _doc, _root) = sample_app();
            app.camera_issue = Some(CameraIssue::NoDevice);
            assert!(!app.feed_active());
            assert_eq!(
                app.no_feed_copy(),
                (
                    "No camera connected",
                    "Connect a camera to start the viewfinder"
                )
            );
            assert!(matches!(app.recovery_action().1, Action::RetryCamera));
        }
    }

    #[test]
    fn storage_sources_never_fabricate_media_or_capture_without_a_feed() {
        // Phase `sample_source_populates_without_touching_disk`.
        {
            let (app, _doc, _root) = sample_app();
            assert_eq!(app.screen, Screen::Camera);
            assert_eq!(app.captures.len(), 1);
            assert!(app.captures.iter().all(|c| c.path.is_none()));
        }
        // Phase `disk_source_starts_empty_and_seeds_no_content`.
        {
            // Point at a directory that does not exist → no fabricated media, never
            // fabricated samples.
            let dir = std::env::temp_dir().join("camera-empty-lib-xyz-does-not-exist");
            let mut app = CameraApp::with(Storage::Disk(dir), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            sync_until(&mut app, &mut doc, &root, |_| true);
            assert!(app.captures.is_empty());
            assert!(app.scans.is_empty());
        }
        // Phase `disk_shutter_without_frame_is_honest`.
        {
            let dir =
                std::env::temp_dir().join(format!("camera-noframe-test-{}", std::process::id()));
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            sync_until(&mut app, &mut doc, &root, |_| true);
            assert!(!app.feed_active());
            app.apply(Action::Shutter, &mut doc, &root);
            // Nothing saved; the toast tells the truth.
            assert!(app.captures.is_empty());
            assert_eq!(app.toast.as_deref(), Some("Connect a camera to capture"));
            assert!(
                !dir.exists(),
                "no directory created when there is nothing to save"
            );
        }
        // Phase `disk_video_record_without_feed_is_honest`.
        {
            let dir =
                std::env::temp_dir().join(format!("camera-vidrec-noframe-{}", std::process::id()));
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            sync_until(&mut app, &mut doc, &root, |_| true);
            app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
            assert!(!app.feed_active());
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(!app.is_recording(), "no feed → recording never starts");
            assert_eq!(app.toast.as_deref(), Some("Connect a camera to record"));
            assert!(!dir.exists());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn photo_shutter_captures_admitted_frames_and_replaces_the_recent_handoff() {
        // Phase `self_timer_is_a_local_cancellable_countdown_before_capture`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::SetTimer(Timer::S3), &mut doc, &root);
            let before = app.captures.len();
            let before_id = app.captures.first().map(|capture| capture.id.clone());
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(app.is_counting_down());
            assert_eq!(app.captures.len(), before, "timer does not capture early");
            app.countdown.as_mut().expect("armed timer").due = ui::time::now();
            app.sync(&mut doc, &root);
            assert!(!app.is_counting_down());
            assert_eq!(app.captures.len(), 1);
            assert_ne!(
                app.captures.first().map(|capture| capture.id.clone()),
                before_id
            );

            app.apply(Action::Shutter, &mut doc, &root);
            assert!(app.is_counting_down());
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(!app.is_counting_down(), "second shutter cancels safely");
        }
        // Phase `sample_shutter_replaces_the_recent_handoff`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = app.captures[0].id.clone();
            app.apply(Action::Shutter, &mut doc, &root);
            assert_eq!(app.captures.len(), 1);
            assert_ne!(app.captures[0].id, before);
            assert!(app.toast.is_some());
        }
        // Phase `disk_shutter_writes_a_real_jpeg_via_handle_event`.
        {
            use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
            use winit::keyboard::{Key, NamedKey, PhysicalKey};

            let dir =
                std::env::temp_dir().join(format!("camera-shutter-test-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            sync_until(&mut app, &mut doc, &root, |_| true);
            assert_eq!(app.screen, Screen::Camera);
            assert_eq!(
                app.settings.photo_format,
                PhotoFormat::Jpeg,
                "default format is JPEG"
            );
            // Simulate a camera frame arriving.
            app.push_camera_frame(frame::viewfinder(1, 64, 48));

            // Press Space through the real event path. `KeyEvent` carries a
            // non-exhaustive field, so build it via the zeroed-struct idiom used
            // across the toolkit's own tests.
            let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = Key::Named(NamedKey::Space);
            ev.physical_key =
                PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            let event = WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            };
            let mods = Modifiers::default();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(handled, "space should be handled on the camera screen");

            // Saving is now bounded background work; pump the real sync boundary
            // until it settles rather than assuming UI event delivery did disk IO.
            let deadline = tilo_test_env::hang_deadline();
            while ui::time::now() < deadline
                && std::fs::read_dir(&dir).map_or(true, |entries| {
                    !entries.flatten().any(|e| {
                        e.path().extension().is_some_and(|x| x == "jpg")
                            && e.metadata().is_ok_and(|m| m.len() >= 2)
                    })
                })
            {
                app.sync(&mut doc, &root);
                std::thread::sleep(Duration::from_millis(1));
            }
            // A real JPEG landed on disk. Camera publishes the portable identity,
            // not the private local projection path.
            let files: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jpg"))
                .collect();
            assert_eq!(files.len(), 1, "shutter wrote exactly one JPEG");
            let bytes = std::fs::read(files[0].path()).unwrap();
            assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "genuine JPEG (SOI marker)");
            sync_until(&mut app, &mut doc, &root, |app| app.captures.len() == 1);
            assert_eq!(app.captures.len(), 1);
            assert!(app.captures[0].path.is_none());
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `malformed_or_exhausted_frames_cannot_replace_the_current_frame`.
        {
            let (mut app, _doc, _root) = sample_app();
            assert!(app.push_camera_frame(frame::viewfinder(1, 32, 24)));
            let prior_generation = app.frame_gen;
            let prior = app.latest_frame.as_ref().unwrap().rgba.clone();
            assert!(!app.push_camera_frame(FrameBuf {
                rgba: vec![0; 3],
                w: 32,
                h: 24,
            }));
            assert_eq!(app.frame_gen, prior_generation);
            assert_eq!(app.latest_frame.as_ref().unwrap().rgba, prior);

            app.frame_gen = u64::MAX;
            assert!(!app.push_camera_frame(frame::viewfinder(2, 32, 24)));
            assert_eq!(app.frame_gen, u64::MAX);
            assert_eq!(app.latest_frame.as_ref().unwrap().rgba, prior);
        }
    }

    #[test]
    fn video_recording_locks_mode_requests_frames_and_saves_a_real_clip() {
        // Phase `set_mode_persists_and_locks_while_recording`.
        {
            let (mut app, mut doc, root) = sample_app();
            assert_eq!(app.mode, CaptureMode::Photo);
            app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
            assert_eq!(app.mode, CaptureMode::Video);
            assert_eq!(doc::read_mode(&doc, &root), CaptureMode::Video);

            // Shutter in video mode starts recording; mode can't be flipped while
            // a clip is in progress.
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(app.is_recording());
            app.apply(Action::SetMode(CaptureMode::Photo), &mut doc, &root);
            assert_eq!(app.mode, CaptureMode::Video, "mode locked while recording");
        }
        // Phase `recording_requests_frames_and_reduced_motion_uses_a_wake_deadline`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(Widget::needs_animation_frame(&app));
            app.set_motion_reduced(true);
            assert!(!Widget::needs_animation_frame(&app));
            assert!(Widget::next_wake_deadline(&app).is_some());
        }
        // Phase `sample_video_record_replaces_the_recent_handoff`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);

            // Start; the hermetic frame-synth runs on each sync tick.
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(app.is_recording());
            for _ in 0..6 {
                app.sync(&mut doc, &root);
            }
            assert!(
                app.frame_gen >= 6,
                "sample frames were admitted to the worker"
            );

            // Stop queues finalization; the bounded worker produces a real clip
            // before sync registers its canonical capture resource.
            app.apply(Action::Shutter, &mut doc, &root);
            assert!(!app.is_recording());
            sync_until(&mut app, &mut doc, &root, |app| {
                app.captures
                    .first()
                    .is_some_and(|capture| capture.id.starts_with("VID_"))
            });
            let clip = app
                .captures
                .iter()
                .find(|capture| capture.id.starts_with("VID_"))
                .expect("finalized sample clip");
            assert!(clip.is_video());
            assert!(clip.dur_ms > 0, "clip has a real duration");
            assert!(
                app.toast
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("Video saved")
            );
        }
        // Phase `disk_video_recording_writes_a_real_avi`.
        {
            let dir =
                std::env::temp_dir().join(format!("camera-vidrec-test-{}", std::process::id()));
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);

            // A frame arrives, then record: start seeds with it, further frames are
            // fed straight into the encoder.
            app.push_camera_frame(frame::viewfinder(0, 128, 96));
            assert!(app.feed_active());
            app.apply(Action::Shutter, &mut doc, &root); // start
            assert!(app.is_recording());
            for i in 1..6u8 {
                app.push_camera_frame(frame::viewfinder(i % 6, 128, 96));
            }
            app.apply(Action::Shutter, &mut doc, &root); // stop → write clip
            assert!(!app.is_recording());

            sync_until(&mut app, &mut doc, &root, |app| app.captures.len() == 1);

            // Exactly one real AVI on disk; it's a RIFF/AVI container that probes.
            let avis: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "avi"))
                .collect();
            assert_eq!(avis.len(), 1, "recording wrote exactly one AVI");
            let bytes = std::fs::read(avis[0].path()).unwrap();
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"AVI ");
            let (w, h, dur) =
                crate::video::probe_avi(&avis[0].path()).expect("probe recorded clip");
            assert_eq!((w, h), (128, 96));
            assert!(dur > 0);

            // Recent captures rescans it as a video with dimensions + duration.
            assert_eq!(app.captures.len(), 1);
            assert!(app.captures[0].is_video());
            assert_eq!((app.captures[0].w, app.captures[0].h), (128, 96));
            assert!(app.captures[0].dur_ms > 0);
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn capture_invocations_publish_only_completed_results() {
        // Phase `cancelled_capture_invocation_cannot_publish_or_leave_a_local_projection`.
        {
            let dir = std::env::temp_dir().join(format!(
                "camera-cancel-capture-{}-{}",
                std::process::id(),
                now_secs()
            ));
            std::fs::remove_dir_all(&dir).ok();
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            sync_until(&mut app, &mut doc, &root, |_| true);
            assert!(app.push_camera_frame(frame::viewfinder(1, 64, 48)));
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::CameraOperation::Capture).unwrap();
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::CAMERA_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &root
                )
                .unwrap(),
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, true, &doc, &root),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            for _ in 0..150 {
                app.sync(&mut doc, &root);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(doc::read_recent_capture(&doc, &root).is_none());
            assert!(std::fs::read_dir(&dir).map_or(true, |entries| {
                entries
                    .flatten()
                    .all(|entry| entry.path().extension().is_none_or(|ext| ext != "jpg"))
            }));
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `cancelled_video_invocation_cannot_publish_or_leave_a_local_projection`.
        {
            let dir = std::env::temp_dir().join(format!(
                "camera-cancel-video-{}-{}",
                std::process::id(),
                now_secs()
            ));
            std::fs::remove_dir_all(&dir).ok();
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            sync_until(&mut app, &mut doc, &root, |_| true);
            app.apply(Action::SetMode(CaptureMode::Video), &mut doc, &root);
            assert!(app.push_camera_frame(frame::viewfinder(1, 128, 96)));
            app.apply(Action::Shutter, &mut doc, &root);
            for seed in 2..8 {
                assert!(app.push_camera_frame(frame::viewfinder(seed, 128, 96)));
            }

            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::CameraOperation::ToggleRecording)
                .expect("video operation payload");
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        invocation,
                        tilo_actions::OperationKind::Effect,
                        crate::actions::CAMERA_OPERATION_TYPE,
                        &payload
                    ),
                    &mut doc,
                    &root
                )
                .expect("accepted video finalization"),
                Some(ui::widget::ResourceOperationStart::Accepted)
            ));
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, true, &doc, &root),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            for _ in 0..200 {
                app.sync(&mut doc, &root);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(doc::read_recent_capture(&doc, &root).is_none());
            assert!(std::fs::read_dir(&dir).map_or(true, |entries| {
                entries
                    .flatten()
                    .all(|entry| entry.path().extension().is_none_or(|ext| ext != "avi"))
            }));
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `sample_document_capture_reports_completion_only_after_adding_the_page`.
        {
            let mut app = CameraApp::with(Storage::Sample, Screen::Document);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            assert!(app.document_scan.pages.is_empty());
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::CameraOperation::CaptureDocumentPage)
                .expect("document capture payload");
            let result = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::CAMERA_OPERATION_TYPE,
                    &payload,
                ),
                &mut doc,
                &root,
            )
            .expect("sample capture succeeds");
            assert!(matches!(
                result,
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
            assert_eq!(app.document_scan.pages.len(), 1);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn document_scanning_captures_edits_and_recognizes_real_pages() {
        // Phase `document_route_captures_real_session_pages_in_sample_mode`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::Nav(Screen::Document), &mut doc, &root);
            assert_eq!(app.screen, Screen::Document);
            assert!(app.document_scan.pages.is_empty());

            app.apply(Action::CaptureDocument, &mut doc, &root);
            assert_eq!(app.document_scan.pages.len(), 1);
            let page = app.document_scan.pages[0].rendered();
            assert_eq!((page.width(), page.height()), (1240, 1754));
            assert_eq!(app.toast.as_deref(), Some("Page 1 captured"));

            app.apply(Action::BeginDocumentCrop, &mut doc, &root);
            assert!(app.document_crop_editor.is_some());
            app.document_crop_selection.set(CropSelection {
                left: 0.1,
                top: 0.1,
                right: 0.9,
                bottom: 0.9,
            });
            app.apply(Action::ApplyDocumentCrop, &mut doc, &root);
            assert!(app.document_crop_editor.is_none());
            let page = app.document_scan.pages[0].rendered();
            assert_eq!((page.width(), page.height()), (992, 1404));

            app.apply(Action::RotateDocument, &mut doc, &root);
            let page = app.document_scan.pages[0].rendered();
            assert_eq!((page.width(), page.height()), (1404, 992));

            let (sender, receiver) = mpsc::channel();
            app.document_job = Some(receiver);
            app.document_job_kind = Some(DocumentJobKind::Recognize);
            app.apply(Action::RemoveDocumentPage, &mut doc, &root);
            assert_eq!(app.document_scan.pages.len(), 1, "busy page is stable");
            sender
                .send(DocumentJobResult::Recognize {
                    page: 0,
                    result: Ok(tilo_ocr::Recognition {
                        document: tilo_ocr::Document {
                            words: Vec::new(),
                            language: "eng".into(),
                        },
                        deskew_degrees: Some(-2.5),
                    }),
                })
                .unwrap();
            app.poll_document_job();
            assert!(app.document_scan.pages[0].ocr.is_some());
            assert_eq!(app.document_scan.pages[0].ocr_deskew_degrees, Some(-2.5));
            assert_eq!(
                app.toast.as_deref(),
                Some("Recognized 0 words, straightened by 2.5°")
            );
            assert!(app.document_job.is_none());

            app.apply(Action::RemoveDocumentPage, &mut doc, &root);
            assert!(app.document_scan.pages.is_empty());
        }
        // Phase `document_capture_never_falls_back_to_a_different_scanner`.
        {
            let (mut app, _doc, _root) = sample_app();
            app.scanner_devices.push(tilo_capture::scanner::Scanner {
                device: "sample:office-scanner".into(),
                description: "Office scanner".into(),
            });
            app.selected_scanner = Some("scanner:disconnected".into());

            app.capture_document_page();
            assert!(app.document_scan.pages.is_empty());
            assert_eq!(app.toast.as_deref(), Some("Choose which scanner to use"));

            app.selected_scanner = Some("sample:office-scanner".into());
            app.capture_document_page();
            assert_eq!(app.document_scan.pages.len(), 1);
            assert_eq!(app.toast.as_deref(), Some("Page 1 captured"));
        }
        // Phase `document_text_status_detects_stale_language_and_copy_uses_real_ocr_text`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::CaptureDocument, &mut doc, &root);
            assert_eq!(app.document_text_status(), "Text not recognized");
            app.document_scan.pages[0].ocr = Some(tilo_ocr::Document {
                words: vec![tilo_ocr::Word {
                    text: "Invoice".into(),
                    confidence: 98.0,
                    bounds: tilo_ocr::Bounds {
                        x: 10,
                        y: 10,
                        width: 80,
                        height: 20,
                    },
                    page: 0,
                    block: 0,
                    paragraph: 0,
                    line: 0,
                    position: 0,
                }],
                language: "eng".into(),
            });
            assert_eq!(app.document_text_status(), "1 word recognized");
            app.document_scan.pages[0].ocr_deskew_degrees = Some(1.5);
            assert_eq!(
                app.document_text_status(),
                "1 word recognized, straightened by 1.5°"
            );
            app.apply(Action::CopyDocumentText, &mut doc, &root);
            assert_eq!(ui::clipboard_paste().as_deref(), Some("Invoice"));
            assert_eq!(app.toast.as_deref(), Some("Recognized text copied"));

            app.document_settings.ocr_language = OcrLanguage::German;
            assert_eq!(app.document_text_status(), "Language changed; update text");
        }
        // Phase `document_ocr_failures_never_expose_engine_diagnostics`.
        {
            let (mut app, _, _) = sample_app();
            let (sender, receiver) = mpsc::channel();
            app.document_job = Some(receiver);
            app.document_job_kind = Some(DocumentJobKind::Recognize);
            sender
                .send(DocumentJobResult::Recognize {
                    page: 0,
                    result: Err(tilo_ocr::Error::Engine(
                        "/private/receipt.png: tesseract internals".into(),
                    )),
                })
                .unwrap();

            app.poll_document_job();
            let message = app.toast.as_deref().unwrap();
            assert!(message.contains("clearer scan"));
            assert!(!message.contains("/private"));
            assert!(!message.contains("tesseract"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn document_choices_update_worker_settings_on_touch_release() {
        // Phase `document_controls_update_the_typed_worker_settings`.
        {
            let (mut app, mut doc, root) = sample_app();
            assert_eq!(app.document_settings, DocumentSettings::default());
            app.scanner_devices.push(tilo_capture::scanner::Scanner {
                device: "sample:office-scanner".into(),
                description: "Office scanner".into(),
            });
            for (choice, selection) in [
                (DocumentChoice::Scanner, 1),
                (DocumentChoice::Resolution, 2),
                (DocumentChoice::Color, 1),
                (DocumentChoice::Source, 1),
                (DocumentChoice::Language, 1),
            ] {
                app.ui_queue
                    .borrow_mut()
                    .push(Action::SetDocumentChoice(choice, selection));
            }
            app.drain_ui_queue(&mut doc, &root);

            assert_eq!(app.document_settings.scan.resolution_dpi, 600);
            assert_eq!(
                app.document_settings.scan.color,
                tilo_capture::scanner::ScanColor::Grayscale
            );
            assert_eq!(
                app.document_settings.scan.source,
                tilo_capture::scanner::ScanSource::Feeder
            );
            assert_eq!(app.document_settings.ocr_language.code(), "deu");
            assert_eq!(
                app.selected_scanner.as_deref(),
                Some("sample:office-scanner")
            );
        }
        // Phase `the_document_screen_hosts_standard_dropdowns`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::Nav(Screen::Document), &mut doc, &root);
            let app = render_at(app, doc, 1280.0, 800.0);
            assert!(app.built_with_document);
            let names: Vec<&str> = app.children().iter().map(|child| child.name()).collect();
            assert_eq!(names.iter().filter(|name| **name == "Dropdown").count(), 5);
            let layout = CameraApp::document_layout(app.rect, &Theme::dark());
            for (field, rect) in app.document_controls.fields.iter().zip(layout.fields) {
                assert!(
                    (field.get_rect().x0 - rect.x0).abs() < 0.5,
                    "each dropdown sits in its slot"
                );
            }
        }
        // Phase `touch_activates_on_release_and_cancels_after_dragging_off`.
        {
            use winit::event::TouchPhase;

            fn touch(phase: TouchPhase, point: Point) -> WindowEvent {
                WindowEvent::Touch(winit::event::Touch {
                    device_id: unsafe { std::mem::zeroed() },
                    phase,
                    location: winit::dpi::PhysicalPosition::new(point.x, point.y),
                    force: None,
                    id: 17,
                })
            }

            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::Nav(Screen::Document), &mut doc, &root);
            let rect = Rect::new(20.0, 20.0, 180.0, 64.0);
            app.regions.push((rect, Action::CaptureDocument));

            assert!(drive(
                &mut app,
                &touch(TouchPhase::Started, rect.center()),
                &mut doc,
                &root
            ));
            assert!(
                app.document_scan.pages.is_empty(),
                "touch-down is not activation"
            );
            assert!(drive(
                &mut app,
                &touch(TouchPhase::Ended, rect.center()),
                &mut doc,
                &root
            ));
            assert_eq!(app.document_scan.pages.len(), 1);

            app.regions.push((rect, Action::CaptureDocument));
            drive(
                &mut app,
                &touch(TouchPhase::Started, rect.center()),
                &mut doc,
                &root,
            );
            drive(
                &mut app,
                &touch(
                    TouchPhase::Moved,
                    Point::new(rect.x1 + 40.0, rect.y1 + 40.0),
                ),
                &mut doc,
                &root,
            );
            drive(
                &mut app,
                &touch(TouchPhase::Ended, rect.center()),
                &mut doc,
                &root,
            );
            assert_eq!(
                app.document_scan.pages.len(),
                1,
                "dragging off cancels the press"
            );
        }
    }

    #[test]
    fn capture_preferences_persist_while_screen_reveals_stay_local() {
        // Phase `screen_reveals_are_presentation_only`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = doc.get_heads();
            app.apply(Action::Nav(Screen::Settings), &mut doc, &root);
            assert_eq!(app.screen, Screen::Settings);
            assert_eq!(doc.get_heads(), before);
            assert!(ui::undo::take_committed_action(&mut app).is_none());
        }
        // Phase `cycle_facing_and_persist`.
        {
            let (mut app, mut doc, root) = sample_app();
            assert_eq!(app.facing, Facing::Back);
            app.apply(Action::CycleFacing, &mut doc, &root);
            assert_eq!(app.facing, Facing::Front);
            assert_eq!(doc::read_facing(&doc, &root), Facing::Front);
        }
        // Phase `toggle_grid_persists_to_settings`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = app.settings.grid;
            app.apply(Action::ToggleGrid, &mut doc, &root);
            assert_eq!(app.settings.grid, !before);
            assert_eq!(doc::read_settings(&doc, &root).grid, !before);
            let dispatched = app
                .test_last_dispatched()
                .expect("grid control dispatches a canonical Camera action");
            let action: crate::actions::CameraAction =
                serde_json::from_slice(&dispatched.payload).expect("Camera action payload");
            assert_eq!(
                action,
                crate::actions::CameraAction::SetGrid { on: !before }
            );
        }
        // Phase `resolution_cycles`.
        {
            let (mut app, mut doc, root) = sample_app();
            assert_eq!(app.settings.resolution, Resolution::R12MP);
            app.apply(Action::CycleResolution, &mut doc, &root);
            assert_eq!(app.settings.resolution, Resolution::R4K);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn scanned_codes_record_history_and_scanner_actions_are_searchable() {
        // Phase `scan_action_records_history`.
        {
            let mut app = CameraApp::sample();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root); // decodes the sample poster → scanned set
            // Force the scanner to decode by rendering once would need a GPU; the
            // sample scanner frame is produced lazily on draw, so seed a scanned
            // code directly to exercise the action path.
            app.scanned = Some(ScannedCode {
                raw: SCAN_PAYLOAD.to_string(),
                kind: classify(SCAN_PAYLOAD),
                symbology: "QR Code".into(),
                corners: [(0.0, 0.0); 4],
            });
            app.apply(Action::Scan(0), &mut doc, &root);
            let scans = doc::read_scans(&doc, &root);
            assert_eq!(scans.len(), 1);
            assert_eq!(scans[0].raw, SCAN_PAYLOAD);
            assert_eq!(app.scans.len(), 1);
        }
        // Phase `wifi_scan_parses_and_primary_action_copies_password`.
        {
            // The Wi-Fi payload is parsed into its fields and the primary action
            // copies the password: the parse and copy path, end to end.
            let payload = "WIFI:T:WPA2;S:Tilo Lounge;P:welcome-aboard;H:false;;";
            let kind = classify(payload);
            match &kind {
                CodeKind::Wifi {
                    ssid,
                    auth,
                    password,
                    hidden,
                } => {
                    assert_eq!(ssid, "Tilo Lounge");
                    assert_eq!(auth, "WPA2");
                    assert_eq!(password, "welcome-aboard");
                    assert!(!hidden);
                    assert_eq!(kind.actions()[0], "Copy password");
                }
                other => panic!("expected wifi, got {other:?}"),
            }
            let mut app = CameraApp::sample();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.scanned = Some(ScannedCode {
                raw: payload.to_string(),
                kind,
                symbology: "QR Code".into(),
                corners: [(0.0, 0.0); 4],
            });
            app.apply(Action::Scan(0), &mut doc, &root); // "Copy password"
            assert_eq!(app.toast.as_deref(), Some("Wi-Fi password copied"));
        }
        // Phase `portal_selected_image_decodes_a_real_qr_without_media_indexing`.
        {
            // Rasterize a real QR to a PNG on disk, then prove the scanner decodes
            // it through the production code path — no camera involved.
            let payload = "https://example.test/scanned-from-a-saved-photo";
            let m = crate::scan::encode(payload).expect("encode");
            let (scale, quiet) = (8usize, 4usize);
            let dim = (m.size + quiet * 2) * scale;
            let mut rgba = vec![255u8; dim * dim * 4];
            for my in 0..m.size {
                for mx in 0..m.size {
                    if !m.is_dark(mx, my) {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let px = (mx + quiet) * scale + dx;
                            let py = (my + quiet) * scale + dy;
                            let i = (py * dim + px) * 4;
                            rgba[i] = 17;
                            rgba[i + 1] = 17;
                            rgba[i + 2] = 17;
                        }
                    }
                }
            }
            let fb = FrameBuf {
                rgba,
                w: dim as u32,
                h: dim as u32,
            };

            let dir = std::env::temp_dir().join(format!("camera-scan-lib-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            // Lossless PNG so the QR survives the write for a clean decode.
            let name = library::write_photo(&dir, &fb, PhotoFormat::Png).unwrap();

            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Scanner);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            assert!(!app.feed_active(), "no camera in the test env");
            assert!(
                app.captures.is_empty(),
                "Camera does not index the directory"
            );
            app.scan_image_path(dir.join(name));
            sync_until(&mut app, &mut doc, &root, |app| app.pending_scan.is_none());
            let scanned = app.scanned.clone().expect("the saved QR decodes");
            assert_eq!(scanned.raw, payload);
            // The decode is recorded to history like a live scan.
            assert_eq!(app.scans.len(), 1);
            assert_eq!(app.scans[0].raw, payload);
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `synchronized_app_searches_and_resolves_canonical_scanner_actions`.
        {
            let (app, doc, _root) = sample_app();
            let providers = ui::search::capability::search_providers(&app);
            assert_eq!(providers.len(), 1);
            let hits = providers[0].search_limited("scan existing image", 4);
            assert_eq!(hits.len(), 1);
            let address = hits[0].action_address().expect("search action");
            assert_eq!(address.action.as_str(), "import-image");
            let resource = ui::resources::resource(&app, &doc, address.resource.as_str())
                .expect("widget resolves search result");
            assert_eq!(resource.id.as_str(), address.resource);
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "import-image")
            );
        }
    }
}
