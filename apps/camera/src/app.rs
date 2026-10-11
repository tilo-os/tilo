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

#[cfg(test)]
use automerge::AutoCommit;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use tilo_clock::unix_now_i64;
use ui::time::{Duration, Instant};
#[cfg(test)]
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::{AppModel, ModelPath};

use automerge::ObjId;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, Setting};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::ActionEmitter;
use ui::widget::toast::ToastAnchor;
use ui::widget::{CropSelection, Image, ImageCropEditor, ImageFit, Popover, Widget};
use winit::event::WindowEvent;

use crate::actions::CameraEffect;
use crate::frame::{self, FrameBuf};
use crate::library;
use crate::model::{
    CameraDevicePreferences, Capture, CaptureKind, CaptureMode, CapturePreferences, CodeKind,
    Facing, PhotoFormat, Resolution, ScanEntry, ScannedCode, Screen, Settings, Timer,
};
use crate::record::Record as Camera;

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
    swipe_start: Option<Point>,
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

    screen: Screen,
    mode: CaptureMode,
    facing: Facing,
    /// The capture preferences on screen: the person's and this device's.
    settings: Settings,
    /// Where they are kept, in a live window.
    preferences: Option<(
        Setting<CapturePreferences>,
        Setting<CameraDevicePreferences>,
    )>,
    /// The record's scan history, as last presented.
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
    /// Set while the file chooser for a document export is open.
    document_export_pending: Rc<Cell<bool>>,
    /// Set while the file chooser for an image to scan is open.
    scan_import_pending: Rc<Cell<bool>>,
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
    pending_capture: Option<(ui::tilo_actions::InvocationId, u64)>,
    pending_video: Option<(ui::tilo_actions::InvocationId, u64)>,
    pending_scan_import: Option<ui::tilo_actions::InvocationId>,
    pending_document_job: Option<ui::tilo_actions::InvocationId>,
    /// The answers owed to the effects whose work is still settling: a
    /// photo or clip being saved, a chosen image being scanned, a document
    /// page being scanned or read.
    replies: HashMap<ui::tilo_actions::InvocationId, EffectReply>,
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

    /// Photos the sample shutter took, registered on the next sync.
    sample_captures: Vec<crate::record::SharedCapture>,
    /// Where the record's edits go, once the framework attached the surface.
    dispatcher: Option<ui::framework::Dispatcher<crate::record::Record>>,
    /// The time a screenshot is taken at, in Unix seconds; `None` reads the
    /// machine's clock.
    clock: Option<i64>,
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

    /// Keep captures in `content`: the store the host lends the window,
    /// which carries them on the camera record's route.
    #[must_use]
    pub fn with_content(mut self, content: tilo_content::ContentStore) -> Self {
        self.content = content;
        self
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
        // Captures are kept in memory until the host lends the window its
        // store ([`Self::with_content`]); the hermetic sample and unit tests
        // never write the user's.
        let content = tilo_content::ContentStore::memory();
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
            screen: initial_screen,
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            settings: Settings::default(),
            preferences: None,
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
            document_export_pending: Rc::default(),
            scan_import_pending: Rc::default(),
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
            replies: HashMap::new(),
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
            sample_captures: Vec::new(),
            dispatcher: Self::unattached_dispatcher(),
            clock: None,
            photos_handoff: Self::media_handoff_emitter("so.tilo.photos", "Photos"),
            photos_browse: Self::photos_browse_emitter(),
            video_handoff: Self::media_handoff_emitter("so.tilo.video-player", "Video Player"),
        }
    }

    fn media_handoff_resource(app_id: &str, label: &str) -> ui::tilo_actions::Resource {
        use ui::tilo_actions::Resource;

        let spec = crate::actions::ApplicationLaunch::content_spec(
            app_id,
            "open",
            &format!("Open {label}"),
            &format!("Open the latest capture in {label}"),
        );
        Resource::new(format!("{app_id}.app"), "applications.app", label).with_action(spec)
    }

    fn media_handoff_emitter(app_id: &str, label: &str) -> Option<ActionEmitter<String>> {
        use ui::tilo_actions::ActionBinding;

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
        use ui::tilo_actions::ActionBinding;

        // Bind to Camera's exported capture root, which owns the routing
        // affordance. The action payload itself delegates to Photos, but the
        // host can still re-resolve this address against the current Camera
        // resource surface before permitting the launch.
        let resource =
            ui::tilo_actions::Resource::new("camera.capture", "camera.capture", "Capture")
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
    pub(crate) fn is_recording(&self) -> bool {
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
            started: self.now_secs(),
        });
        self.dirty = true;
    }

    /// Finish the recording by submitting its AVI mux/verification to the
    /// bounded finalizer. `sync` publishes the resulting canonical capture.
    fn stop_recording(
        &mut self,
        invocation: Option<ui::tilo_actions::InvocationId>,
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
                    .disabled(self.scan_import_pending.get()),
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

    /// Where edits go before the framework attaches the surface: nowhere.
    #[cfg(not(test))]
    fn unattached_dispatcher() -> Option<ui::framework::Dispatcher<crate::record::Record>> {
        None
    }

    /// A widget a test builds edits the document the test hands it.
    #[cfg(test)]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same signature as the build without tests, where nothing is attached yet"
    )]
    fn unattached_dispatcher() -> Option<ui::framework::Dispatcher<crate::record::Record>> {
        Some(ui::framework::Dispatcher::detached(
            "so.tilo.camera",
            "test-author",
        ))
    }

    /// Invoke `effect` as the row `row` offers it, through the same door as
    /// the palette and agents: a button on screen runs exactly what they do.
    fn invoke_on(&mut self, row: &str, effect: &CameraEffect) {
        match &self.dispatcher {
            Some(dispatcher) => dispatcher.effect_on(row, effect),
            None => self.toast = Some("Camera is not open on a record".into()),
        }
    }

    /// Edit the record through `dispatcher` from now on.
    pub(crate) fn attach_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::record::Record>,
    ) {
        self.dispatcher = Some(dispatcher);
    }

    /// Read the time from `now` (Unix seconds) instead of the machine's
    /// clock, as a screenshot does.
    pub(crate) fn set_clock(&mut self, now: i64) {
        self.clock = Some(now);
    }

    /// Stage a screenshot scenario (`None` for a plain `--shots`): the
    /// screen it shows and what is happening on it. Mode, timer and the
    /// recent capture are the record's already.
    pub(crate) fn stage_scene(&mut self, scenario: Option<&str>) {
        match scenario.unwrap_or_default() {
            "scanner" => self.screen = Screen::Scanner,
            "document" => self.screen = Screen::Document,
            "document-page" => {
                self.screen = Screen::Document;
                self.capture_document_page();
            }
            "document-crop" => {
                self.screen = Screen::Document;
                self.capture_document_page();
                self.begin_document_crop();
            }
            "settings" => self.screen = Screen::Settings,
            "settings-scrolled" => {
                self.screen = Screen::Settings;
                // The retained settings dialog scrolls its form when space is limited.
            }
            "timer" => {
                self.countdown = Some(Countdown {
                    due: ui::time::now() + Duration::from_secs(3),
                    mode: self.mode,
                });
            }
            "no-camera" => self.camera_issue = Some(CameraIssue::NoDevice),
            "camera-permission" => self.camera_issue = Some(CameraIssue::PermissionDenied),
            "camera-error" => self.camera_issue = Some(CameraIssue::StartFailed),
            _ => {}
        }
        self.dirty = true;
    }

    /// The current time in Unix seconds.
    fn now_secs(&self) -> i64 {
        self.clock.unwrap_or_else(unix_now_i64)
    }

    /// Apply one of the record's verbs — the single mutation path shared
    /// with the palette and agents — to the document the event was handed,
    /// through the framework's dispatcher: checked, attributed, one undo
    /// step, replicated.
    fn perform_action(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::record::Record>,
    ) {
        if let Err(error) = self.try_perform_action(doc, submit) {
            self.toast = Some(format!("Couldn't save change: {error}"));
        }
    }

    /// Dispatch where the caller must not continue after a refused
    /// mutation (worker settlement and destructive resource effects).
    fn try_perform_action(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::record::Record>,
    ) -> Result<(), String> {
        let Some(dispatcher) = &self.dispatcher else {
            return Err("Camera is not open on a record".into());
        };
        dispatcher
            .submit_now(doc, submit)
            .map_err(|error| error.to_string())
    }

    /// Record the scanned code `raw` in the history, and show the history
    /// it leaves.
    fn record_scan(&mut self, doc: &mut ui::widget::Edits<'_>, raw: String) {
        self.perform_action(doc, crate::record::Record::RECORD_SCAN.with(raw));
        if let Ok(record) = <crate::record::Record as ui::tilo_resource::Resource>::read(doc) {
            self.scans = record.scans;
        }
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
            scan_import_pending: self.scan_import_pending.get(),
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

    fn apply(&mut self, action: Action, doc: &mut ui::widget::Edits<'_>, root: &ObjId) {
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
            Action::SetMode(m) => self.apply_set_mode(m),
            Action::CycleFacing => self.apply_cycle_facing(),
            Action::DismissScan => self.scan_dismissed = true,
            Action::Scan(i) => self.apply_scan_action(i, doc),
            Action::ImportScanImage => {
                self.invoke_on(crate::actions::SCANNER_ROW, &CameraEffect::ImportImage);
            }
            Action::RetryCamera => self.retry_camera(),
            Action::OpenCameraPrivacySettings => self.open_camera_privacy_settings(),
            Action::OpenCameraChooser => self.open_camera_chooser(),
            Action::SelectCamera(device) => self.select_camera(&device),
            Action::CaptureDocument => self.capture_document_page(),
            Action::SetDocumentChoice(choice, index) => self.apply_document_choice(choice, index),
            Action::RotateDocument => self.rotate_document_page(),
            Action::RecognizeDocument => self.recognize_document_page(),
            Action::CopyDocumentText => self.copy_document_text(),
            Action::ExportDocument => {
                self.invoke_on(crate::actions::DOCUMENT_ROW, &CameraEffect::ExportPdf);
            }
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
                self.apply_settings_toggle(&settings_action);
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
    fn drain_ui_queue(&mut self, doc: &mut ui::widget::Edits<'_>, root: &ObjId) {
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

    fn apply_nav(&mut self, s: Screen, _doc: &mut ui::widget::Edits<'_>, _root: &ObjId) {
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
                    self.settle(
                        invocation,
                        Err((
                            "camera.document_worker_stopped",
                            "Document processing stopped unexpectedly".into(),
                            true,
                        )),
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
            self.settle(
                invocation,
                terminal_error.map_or_else(
                    || Ok(ui::tilo_actions::ActionOutput::default()),
                    |(code, message)| Err((code, message, true)),
                ),
            );
        }
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

    /// Ask where to save the scanned pages as one searchable PDF, as the
    /// export effect does.
    fn export_document(&self, cx: &mut EffectCx<Camera, Self>) -> EffectAnswer {
        if self.document_scan.pages.is_empty() {
            return Err("Capture a page before exporting".into());
        }
        let Some(open) = ChooserOpen::open(&self.document_export_pending) else {
            return Err("The save location chooser is already open".into());
        };
        Ok(cx.save_file(
            ui::portal::PickRequest::save("Save scanned document", "Save", "Scanned document.pdf"),
            self.document_scan.searchable_pdf(),
            move |app, saved, _| {
                drop(open);
                app.dirty = true;
                if saved {
                    app.toast = Some("Saved scanned document".into());
                    Ok(Effected::done())
                } else {
                    Err("Document export cancelled".into())
                }
            },
        ))
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

    fn apply_back(&mut self, _doc: &mut ui::widget::Edits<'_>, _root: &ObjId) {
        self.screen = self.return_to;
    }

    fn apply_shutter(&mut self, _doc: &mut ui::widget::Edits<'_>, _root: &ObjId) {
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

    fn apply_set_mode(&mut self, m: CaptureMode) {
        // Switching mode mid-record would strand the clip; ignore.
        if !self.is_recording() && !self.is_counting_down() {
            self.mode = m;
            // Photo/Video also bring the viewfinder forward when chosen
            // from the scanner's shared switcher.
            if self.screen != Screen::Camera {
                self.screen = Screen::Camera;
            }
        }
    }

    fn apply_cycle_facing(&mut self) {
        self.use_lens(self.facing.next());
    }

    /// Look through `facing`.
    fn use_lens(&mut self, facing: Facing) {
        if facing != self.facing {
            self.facing = facing;
            self.cam_view = None;
        }
    }

    fn apply_scan_action(&mut self, i: usize, doc: &mut ui::widget::Edits<'_>) {
        if let Some(s) = self.scanned.clone() {
            // The chosen action (open / connect / copy) is external; the
            // scan-history row-insert is the collaborative action.
            self.perform_scan_action(&s, i);
            self.record_scan(doc, s.raw.clone());
        }
    }

    fn apply_settings_toggle(&mut self, action: &Action) {
        self.change_settings(|settings| match action {
            Action::CycleResolution => {
                settings.resolution = cycle(&Resolution::ALL, settings.resolution);
            }
            Action::SetPhotoFormat(format) => settings.photo_format = *format,
            Action::SetResolution(resolution) => settings.resolution = *resolution,
            Action::SetTimer(tm) => settings.timer = *tm,
            Action::ToggleGrid => settings.grid = !settings.grid,
            Action::ToggleLevel => settings.level = !settings.level,
            Action::ToggleMirror => settings.mirror_front = !settings.mirror_front,
            Action::ToggleGeotag => settings.geotag = !settings.geotag,
            Action::ToggleHdr => settings.hdr = !settings.hdr,
            Action::SetDefaultFacing(facing) => settings.default_facing = *facing,
            _ => {}
        });
    }

    /// Change the capture preferences on screen and keep them: the person's
    /// and this device's settings in a live window.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.dirty = true;
        let Some((person, device)) = &self.preferences else {
            return;
        };
        let kept = person
            .set(self.settings.person())
            .and_then(|()| device.set(self.settings.device()));
        if let Err(error) = kept {
            self.toast = Some(format!("Couldn't keep the setting: {error}"));
        }
    }

    /// Follow the person's and this device's capture preferences, opening
    /// on the default lens.
    pub(crate) fn follow_preferences(
        &mut self,
        person: Setting<CapturePreferences>,
        device: Setting<CameraDevicePreferences>,
    ) {
        self.settings = Settings::from_parts(person.get(), device.get());
        self.facing = self.settings.default_facing;
        self.preferences = Some((person, device));
    }

    /// Show `person` and `device` preferences a capture stages.
    pub(crate) fn stage_preferences(
        &mut self,
        person: CapturePreferences,
        device: CameraDevicePreferences,
    ) {
        self.settings = Settings::from_parts(person, device);
    }

    /// Film or photograph, as a capture stages.
    pub(crate) fn stage_mode(&mut self, mode: CaptureMode) {
        self.mode = mode;
    }

    /// The capture mode on screen.
    pub fn capture_mode(&self) -> CaptureMode {
        self.mode
    }

    /// The capture preferences on screen.
    pub fn capture_settings(&self) -> Settings {
        self.settings
    }

    /// The record's scan history and recent capture, as presented.
    pub(crate) fn present_record(&mut self, state: &crate::record::Record) {
        self.scans.clone_from(&state.scans);
        // Camera retains exactly one collaborative handoff. Photos and Video
        // Player own collection browsing, deletion, favorites, and metadata.
        // The sample storage has no bytes for it and draws a procedural scene.
        let scene = u8::from(matches!(self.storage, Storage::Sample));
        let next = state.recent.clone().map(|shared| Capture {
            id: shared.id,
            kind: shared.kind,
            ts: shared.ts,
            w: shared.w,
            h: shared.h,
            dur_ms: shared.dur_ms,
            label: shared.label,
            facing: shared.facing,
            scene,
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
        }
        self.dirty = true;
    }

    /// Take a photo: write the current frame to disk (live) or append an
    /// in-memory sample (hermetic).
    fn capture(
        &mut self,
        invocation: Option<ui::tilo_actions::InvocationId>,
    ) -> Result<(), String> {
        match &self.storage {
            Storage::Sample => {
                let frame = frame::thumbnail(Self::viewfinder_seed(self.facing), 64, 48);
                let (_, bytes) = library::encode_photo(&frame, self.settings.photo_format)
                    .map_err(|error| error.to_string())?;
                let media_id = crate::media::store(&self.content, &bytes)
                    .ok_or_else(|| "the sample photo was not stored".to_owned())?;
                let (w, h) = self.settings.resolution.dims();
                self.sample_captures.push(crate::record::SharedCapture {
                    id: format!(
                        "IMG_{}.{}",
                        self.now_secs(),
                        self.settings.photo_format.extension()
                    ),
                    media_id,
                    kind: CaptureKind::Photo,
                    ts: self.now_secs(),
                    w,
                    h,
                    dur_ms: 0,
                    label: "New capture".into(),
                    facing: self.facing,
                });
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

    /// Answer one [`CameraEffect`], invoked by a control, the palette, an
    /// agent or a script. Work that settles later — a photo or clip being
    /// saved, a chosen image being scanned, a document page being scanned or
    /// read — answers its caller when it does.
    pub(crate) fn answer_effect(
        &mut self,
        effect: CameraEffect,
        cx: &mut EffectCx<Camera, Self>,
    ) -> EffectAnswer {
        self.dirty = true;
        match effect {
            CameraEffect::OpenCamera => self.screen = Screen::Camera,
            CameraEffect::OpenScanner => self.screen = Screen::Scanner,
            CameraEffect::OpenDocument => self.screen = Screen::Document,
            CameraEffect::OpenSettings => self.screen = Screen::Settings,
            CameraEffect::UseMode { mode } => {
                if self.is_recording() {
                    return Err("Stop recording to switch modes".into());
                }
                self.apply_set_mode(CaptureMode::parse_str(&mode));
            }
            CameraEffect::UseLens { lens } => self.use_lens(Facing::parse_str(&lens)),
            CameraEffect::SetResolution { resolution } => {
                let resolution = Resolution::parse_str(&resolution);
                self.change_settings(|settings| settings.resolution = resolution);
            }
            CameraEffect::SetPhotoFormat { format } => {
                let format = PhotoFormat::parse_str(&format);
                self.change_settings(|settings| settings.photo_format = format);
            }
            CameraEffect::SetTimer { seconds } => {
                let timer = Timer::from_seconds(seconds.trim().parse().unwrap_or(0));
                self.change_settings(|settings| settings.timer = timer);
            }
            CameraEffect::SetGrid { on } => self.change_settings(|settings| settings.grid = on),
            CameraEffect::SetLevel { on } => self.change_settings(|settings| settings.level = on),
            CameraEffect::SetMirrorFront { on } => {
                self.change_settings(|settings| settings.mirror_front = on);
            }
            CameraEffect::SetGeotag { on } => {
                self.change_settings(|settings| settings.geotag = on);
            }
            CameraEffect::SetHdr { on } => self.change_settings(|settings| settings.hdr = on),
            CameraEffect::SetDefaultLens { lens } => {
                let facing = Facing::parse_str(&lens);
                self.change_settings(|settings| settings.default_facing = facing);
            }
            CameraEffect::Capture => {
                self.capture(cx.invocation())?;
                if self.pending_capture.is_some() {
                    return Ok(self.answer_later(cx));
                }
            }
            CameraEffect::Record => {
                if self.is_recording() {
                    // Both live and sample recordings finalize through the
                    // bounded worker, so the caller hears back only once the
                    // clip is muxed, verified and registered.
                    self.stop_recording(cx.invocation())?;
                    if self.pending_video.is_some() {
                        return Ok(self.answer_later(cx));
                    }
                } else {
                    self.start_recording();
                }
            }
            CameraEffect::RetryCamera => self.retry_camera(),
            CameraEffect::OpenCameraPrivacy => self.open_camera_privacy_settings(),
            CameraEffect::UseCamera { device } => self.select_camera(&device),
            CameraEffect::ImportImage => return self.import_scan_image(cx),
            CameraEffect::CapturePage => {
                let pages_before = self.document_scan.pages.len();
                self.capture_document_page();
                if self.document_job.is_some() {
                    self.pending_document_job = cx.invocation();
                    return Ok(self.answer_later(cx));
                }
                if self.document_scan.pages.len() == pages_before {
                    return Err("No document scanner is available".into());
                }
            }
            CameraEffect::RotatePage => self.rotate_document_page(),
            CameraEffect::RecognizeText => {
                self.recognize_document_page();
                if self.document_job.is_none() {
                    return Err("Text recognition could not start".into());
                }
                self.pending_document_job = cx.invocation();
                return Ok(self.answer_later(cx));
            }
            CameraEffect::CopyText => self.copy_document_text(),
            CameraEffect::ExportPdf => return self.export_document(cx),
            CameraEffect::CropPage => self.begin_document_crop(),
            CameraEffect::MovePageEarlier => self.move_document_page(false),
            CameraEffect::MovePageLater => self.move_document_page(true),
            CameraEffect::RemovePage => self.remove_document_page(),
        }
        Ok(Effected::done())
    }

    /// Owe the caller of the effect `cx` answers its answer until the work
    /// it started settles ([`Self::settle`]).
    fn answer_later(&mut self, cx: &mut EffectCx<Camera, Self>) -> Effected {
        if let Some(invocation) = cx.invocation() {
            self.replies.insert(invocation, cx.reply());
        }
        Effected::Pending
    }

    /// Answer the caller waiting for `invocation`, if any still is.
    fn settle(
        &mut self,
        invocation: ui::tilo_actions::InvocationId,
        result: Result<ui::tilo_actions::ActionOutput, Failure<'_>>,
    ) {
        if let Some(reply) = self.replies.remove(&invocation) {
            reply.answer(result.map_err(|(code, message, retryable)| {
                ui::tilo_actions::ActionFailure {
                    code: code.into(),
                    message,
                    details: None,
                    retryable,
                }
            }));
        }
    }

    /// Drop the work whose caller cancelled it: a photo or clip still being
    /// saved is discarded when it arrives (with its local file), a scan or
    /// a document task no longer answers anyone.
    fn forget_cancelled(&mut self) {
        let cancelled: Vec<_> = self
            .replies
            .iter()
            .filter(|(_, reply)| reply.is_cancelled())
            .map(|(invocation, _)| *invocation)
            .collect();
        for invocation in cancelled {
            self.replies.remove(&invocation);
            if self
                .pending_capture
                .is_some_and(|(pending, _)| pending == invocation)
            {
                self.pending_capture = None;
            }
            if self
                .pending_video
                .is_some_and(|(pending, _)| pending == invocation)
            {
                self.pending_video = None;
            }
            if self.pending_scan_import == Some(invocation) {
                self.pending_scan_import = None;
                self.pending_scan = None;
            }
            if self.pending_document_job == Some(invocation) {
                self.pending_document_job = None;
                self.document_job = None;
                self.document_job_kind = None;
            }
        }
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

    /// Ask for an image with the file chooser and scan the code in it, as
    /// the import effect does. The caller hears back once the scan settles.
    fn import_scan_image(&mut self, cx: &mut EffectCx<Camera, Self>) -> EffectAnswer {
        let Some(open) = ChooserOpen::open(&self.scan_import_pending) else {
            return Err("An image picker is already open".into());
        };
        self.toast = Some("Choose an image containing a QR code or barcode".into());
        let request = ui::portal::PickRequest::open("Scan a code from an image", "Scan").filter(
            ui::portal::FileFilter::new("Images")
                .mimetype("image/png")
                .mimetype("image/jpeg")
                .mimetype("image/webp")
                .mimetype("image/gif"),
        );
        Ok(cx.choose_file(request, move |app, chosen, cx| {
            drop(open);
            app.dirty = true;
            let Some(file) = chosen else {
                app.toast = Some("Image scan cancelled".into());
                return Err("Image scan cancelled".into());
            };
            app.scan_image_bytes(file.bytes)?;
            app.pending_scan_import = cx.invocation();
            Ok(app.answer_later(cx))
        }))
    }

    /// Decode a code from the bytes of one chosen image. The image never
    /// enters Camera's collaborative state or a private media index.
    fn scan_image_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        self.scan_dismissed = false;
        let Some(generation) = self.scan_generation.checked_add(1) else {
            let error = "Code scan counter exhausted; restart Camera".to_owned();
            self.toast = Some(error.clone());
            return Err(error);
        };
        let id = format!("portal:{generation}");
        if let Err(error) = self.worker.submit_scan(crate::worker::ScanJob {
            generation,
            id: id.clone(),
            source: crate::worker::ScanSource::Bytes(bytes),
            record_history: true,
        }) {
            self.toast = Some(error.clone());
            return Err(error);
        }
        self.scan_generation = generation;
        self.pending_scan = Some((id, generation));
        self.toast = Some("Scanning image…".into());
        Ok(())
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

    fn hit(&mut self, pos: Point, doc: &mut ui::widget::Edits<'_>, root: &ObjId) -> bool {
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
            } else {
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

/// A failure told to an effect's caller: a code for programs, a message for
/// people, and whether trying again may help.
type Failure<'a> = (&'a str, String, bool);

/// A file chooser an effect opened, marked in `open` until it answers or
/// its job ends without an answer (a failure, a cancelled effect), so the
/// screen and the published effect show it is open meanwhile.
struct ChooserOpen(Rc<Cell<bool>>);

impl ChooserOpen {
    /// Mark `open`, unless a chooser is open already.
    fn open(open: &Rc<Cell<bool>>) -> Option<Self> {
        (!open.replace(true)).then(|| Self(Rc::clone(open)))
    }
}

impl Drop for ChooserOpen {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

fn frame_image_fill(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Fill)
}

fn frame_image_cover(buf: FrameBuf) -> Image {
    let (w, h) = (buf.w, buf.h);
    Image::from_rgba(Arc::from(buf.rgba), w, h).with_fit(ImageFit::Cover)
}

impl CameraApp {
    fn poll_photo_results(&mut self, state: &mut ui::widget::Edits<'_>) {
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
                    let action = crate::record::SharedCapture {
                        id: saved.name.clone(),
                        media_id: saved.media_id.clone(),
                        kind: CaptureKind::Photo,
                        ts: self.now_secs(),
                        w: saved.w,
                        h: saved.h,
                        dur_ms: 0,
                        label: "New capture".into(),
                        facing: saved.facing,
                    };
                    if let Err(error) = self.try_perform_action(
                        state,
                        crate::record::Record::REGISTER_CAPTURE.with(action),
                    ) {
                        if let Some(path) = &saved.local_path {
                            crate::files::remove(path);
                        }
                        self.toast = Some(format!("Save failed: {error}"));
                        if let Some(invocation) = terminal {
                            self.settle(invocation, Err(("camera.register_refused", error, false)));
                        }
                        continue;
                    }
                    let output = ui::tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "media_id": saved.media_id.clone(),
                            "capture_id": saved.name.clone(),
                        })),
                        resources: vec![format!("camera.photo:{}", saved.name.clone())],
                    };
                    self.toast = Some("Photo saved".into());
                    if let Some(invocation) = terminal {
                        self.settle(invocation, Ok(output));
                    }
                }
                Err(error) => {
                    self.toast = Some(format!("Save failed: {error}"));
                    if let Some(invocation) = terminal {
                        self.settle(invocation, Err(("camera.save_failed", error, true)));
                    }
                }
            }
        }
    }

    fn poll_video_results(&mut self, state: &mut ui::widget::Edits<'_>) {
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
                    let action = crate::record::SharedCapture {
                        id: saved.name.clone(),
                        media_id: saved.media_id.clone(),
                        kind: CaptureKind::Video,
                        ts: self.now_secs(),
                        w: saved.w,
                        h: saved.h,
                        dur_ms: saved.dur_ms,
                        label: "New clip".into(),
                        facing: saved.facing,
                    };
                    if let Err(error) = self.try_perform_action(
                        state,
                        crate::record::Record::REGISTER_CAPTURE.with(action),
                    ) {
                        if let Some(path) = &saved.local_path {
                            crate::files::remove(path);
                        }
                        self.toast = Some(format!("Save failed: {error}"));
                        if let Some(invocation) = terminal {
                            self.settle(invocation, Err(("camera.register_refused", error, false)));
                        }
                        continue;
                    }
                    let output = ui::tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "media_id": saved.media_id.clone(),
                            "capture_id": saved.name.clone(),
                        })),
                        resources: vec![format!("camera.video:{}", saved.name.clone())],
                    };
                    self.toast = Some("Video saved".into());
                    if let Some(invocation) = terminal {
                        self.settle(invocation, Ok(output));
                    }
                }
                Err(error) => {
                    self.toast = Some(format!("Save failed: {error}"));
                    if let Some(invocation) = terminal {
                        self.settle(invocation, Err(("camera.video_failed", error, true)));
                    }
                }
            }
        }
    }

    fn poll_thumbnail_results(&mut self) {
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
    }

    fn poll_scan_results(&mut self, state: &mut ui::widget::Edits<'_>) {
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
                        self.record_scan(state, scanned.raw);
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
                self.settle(
                    invocation,
                    terminal_error.map_or_else(
                        || Ok(ui::tilo_actions::ActionOutput::default()),
                        |(code, message, retryable)| Err((code, message, retryable)),
                    ),
                );
            }
            self.dirty = true;
        }
    }
}

impl Widget for CameraApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        // Another window, or another of the person's devices, changed the
        // capture preferences.
        if let Some((person, device)) = &self.preferences {
            let kept = Settings::from_parts(person.get(), device.get());
            if kept != self.settings {
                self.settings = kept;
                self.dirty = true;
            }
        }
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        #[cfg(feature = "live-capture")]
        self.poll_camera();

        // Hermetic path has no frame clock: while recording, synthesize a
        // procedural frame each tick so a real multi-frame clip accumulates.
        if self.recording.is_some() && matches!(self.storage, Storage::Sample) {
            let seed = Self::viewfinder_seed(self.facing).wrapping_add((self.frame_gen as u8) & 3);
            let fb = frame::viewfinder(seed, 480, 270);
            let _ = self.push_camera_frame(fb);
            self.dirty = true;
        }
        self.dirty
    }

    fn settle(&mut self, state: &mut ui::widget::Edits<'_>, _path: &ObjId) {
        self.forget_cancelled();
        self.poll_document_job();
        // An empty record already reads as Photo / Back / default settings.
        // Writing those defaults would be a phantom history entry.
        self.poll_countdown();
        self.poll_photo_results(state);
        self.poll_video_results(state);
        self.poll_thumbnail_results();
        self.poll_scan_results(state);
        for capture in std::mem::take(&mut self.sample_captures) {
            if let Err(error) = self
                .try_perform_action(state, crate::record::Record::REGISTER_CAPTURE.with(capture))
            {
                self.toast = Some(format!("Save failed: {error}"));
            }
        }
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
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        // A toast never covers what it reports on: over the viewfinder it
        // rises above the shutter row; on the scanner and the documents,
        // whose lower half is content, it stands at the top beside the
        // corner cluster.
        let (toast_area, toast_anchor) = match self.base_screen() {
            Screen::Camera | Screen::Settings => {
                let controls_top = self.draw_camera(scene, t, tr, theme, full);
                (
                    Rect::new(full.x0, full.y0, full.x1, controls_top),
                    ToastAnchor::BottomCenter,
                )
            }
            Screen::Scanner => {
                let corner = self.draw_scanner(scene, t, tr, theme, full);
                (
                    hud::top_band_toast_area(full, corner, theme),
                    ToastAnchor::TopCenter,
                )
            }
            Screen::Document => {
                let corner = self.draw_document_scanner(scene, t, tr, theme, presence, full);
                (
                    hud::top_band_toast_area(full, corner, theme),
                    ToastAnchor::TopCenter,
                )
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
        self.draw_toast(scene, t, tr, theme, toast_area, toast_anchor);
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
                &mut ctx.state,
                path,
            );
            *ctx.handled = true;
            self.drain_ui_queue(&mut ctx.state, path);
            self.dirty = true;
            return;
        }
        if self.chooser.is_open() {
            self.chooser.handle_event(event, ctx);
            if let Some(index) = self.chooser.take_selection()
                && let Some(device) = self.chooser.items().get(index).cloned()
            {
                self.apply(Action::SelectCamera(device), &mut ctx.state, path);
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
            self.drain_ui_queue(&mut ctx.state, path);
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
                if self.hit(self.last_mouse, &mut ctx.state, ctx.path) {
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
                                swipe_start: Some(point),
                            });
                            *ctx.handled = true;
                        } else if self.screen != Screen::Settings && self.rect.contains(point) {
                            self.active_touch = Some(TouchTarget {
                                id: touch.id,
                                rect: self.rect,
                                action: Action::Nav(self.base_screen()),
                                cancelled: false,
                                swipe_start: Some(point),
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
                            if let Some(target) = target {
                                let mode_target = matches!(
                                    target.action,
                                    Action::SetMode(_)
                                        | Action::Nav(
                                            Screen::Camera | Screen::Scanner | Screen::Document
                                        )
                                );
                                let swipe = (mode_target
                                    && !self.is_recording()
                                    && !self.is_counting_down())
                                .then(|| {
                                    target.swipe_start.and_then(|start| {
                                        ui::widget::ToggleGroup::swipe_selection(
                                            point - start,
                                            f64::from(
                                                self.layout_theme.tokens.metrics.min_pointer_target,
                                            ),
                                            self.selected_capture_mode(),
                                            4,
                                        )
                                    })
                                })
                                .flatten();
                                if let Some(mode) = swipe {
                                    let action = match mode {
                                        0 => Action::SetMode(CaptureMode::Photo),
                                        1 => Action::SetMode(CaptureMode::Video),
                                        2 => Action::Nav(Screen::Scanner),
                                        _ => Action::Nav(Screen::Document),
                                    };
                                    self.apply(action, &mut ctx.state, ctx.path);
                                } else if !target.cancelled && target.rect.contains(point) {
                                    self.apply(target.action, &mut ctx.state, ctx.path);
                                }
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
                    TouchPhase::Started => {
                        if let Some(target) = self.active_touch.as_mut() {
                            target.swipe_start = None;
                            target.cancelled = true;
                        }
                    }
                    TouchPhase::Cancelled => {}
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
                    self.apply(a, &mut ctx.state, ctx.path);
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
    fn wants_pointer_capture(&self) -> bool {
        self.active_touch.is_some()
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        if self.screen == Screen::Settings || self.chooser.is_open() || !self.rect.contains(point) {
            return false;
        }
        // Shared fields and the crop editor retain their ordinary host input
        // path; only the manually painted camera surfaces own mode swipes.
        if self.screen == Screen::Document
            && (self.document_crop_editor.is_some()
                || self
                    .document_controls
                    .fields
                    .iter()
                    .any(|field| field.is_open() || field.get_rect().contains(point)))
        {
            return false;
        }
        true
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_idle_sync(&self) -> bool {
        self.document_export_pending.get()
            || self.scan_import_pending.get()
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
        state: &mut ui::widget::Edits<'_>,
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
}

impl ui::widget::controls::ActionControls for CameraApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
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
        allowed: &HashSet<ui::tilo_actions::ActionAddress>,
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

// ====================================================================
// Shared chrome + control helpers
// ====================================================================

fn now_secs() -> i64 {
    ui::clock::local_now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::classify;
    use ui::headless::{Framed, viewport};

    #[test]
    fn switch_camera_is_an_accessible_symbol_beside_the_shutter() {
        for width in [390.0, 1280.0] {
            let (app, doc, _) = sample_app();
            let frame = render_at(app, doc, width, 844.0);
            let rect = |find: fn(&Action) -> bool| {
                frame
                    .regions
                    .iter()
                    .find(|(_, action)| find(action))
                    .map(|(rect, _)| *rect)
                    .unwrap()
            };
            let shutter = rect(|action| matches!(action, Action::Shutter));
            let switch = rect(|action| matches!(action, Action::CycleFacing));
            assert!(switch.x0 > shutter.x1 && switch.x1 <= f64::from(width));
            assert_eq!(
                switch.center().y.total_cmp(&shutter.center().y),
                std::cmp::Ordering::Equal
            );
            assert!(switch.x0 - shutter.x1 <= f64::from(Theme::dark().tokens.spacing.s4));
            let tree = frame.accessibility().unwrap();
            let node = tree
                .children
                .iter()
                .find(|node| node.action_target.as_deref() == Some("camera.facing"))
                .unwrap();
            assert_eq!(node.role, Role::Button);
            assert!(node.name.as_deref().unwrap().starts_with("Switch camera"));
            assert!(node.state.focusable);
        }
    }

    #[test]
    fn document_dropdowns_keep_real_hosted_touch_activation() {
        use winit::event::TouchPhase::{Ended, Started};
        let (mut app, mut doc, root) = sample_app();
        app.apply(
            Action::Nav(Screen::Document),
            &mut Edits::lend(&mut doc),
            &root,
        );
        let mut frame = render_at(app, doc, 390.0, 844.0);
        let rect = frame.document_controls.fields[0].get_rect();
        assert!(rect.width() > 0.0 && rect.height() > 0.0);
        assert!(
            !frame.claims_raw_touch(rect.center()),
            "shared dropdown owns host pointer emulation"
        );
        let harness = Framed::harness_mut(&mut frame);
        harness.touch(1, Started, rect.center().x, rect.center().y);
        harness.touch(1, Ended, rect.center().x, rect.center().y);
        assert!(
            frame.document_controls.fields[0].is_open(),
            "a real touch opens the retained dropdown"
        );
    }

    #[test]
    fn mode_swipes_use_real_camera_actions_and_cancel_without_capturing() {
        use winit::event::TouchPhase::{Cancelled, Ended, Started};
        let (app, doc, _) = sample_app();
        let mut frame = render_at(app, doc, 390.0, 844.0);
        let captures = frame.captures.len();
        // The viewfinder supports the same gesture as the mode strip.
        let start = Point::new(195.0, 300.0);
        let harness = Framed::harness_mut(&mut frame);
        harness.touch(1, Started, start.x, start.y);
        harness.touch(1, Ended, start.x - 90.0, start.y);
        assert_eq!(frame.mode, CaptureMode::Video);
        assert!(!frame.is_recording(), "changing modes never starts capture");
        let harness = Framed::harness_mut(&mut frame);
        harness.touch(2, Started, start.x, start.y);
        harness.touch(2, Cancelled, start.x - 90.0, start.y);
        harness.touch(2, Ended, start.x - 90.0, start.y);
        assert_eq!(frame.mode, CaptureMode::Video);
        let harness = Framed::harness_mut(&mut frame);
        harness.touch(3, Started, start.x, start.y);
        harness.touch(3, Ended, start.x - 10.0, start.y + 90.0);
        assert_eq!(
            frame.screen,
            Screen::Camera,
            "vertical gestures do not navigate"
        );
        let harness = Framed::harness_mut(&mut frame);
        harness.touch(4, Started, start.x, start.y);
        harness.touch(4, Ended, start.x - 90.0, start.y);
        assert_eq!(frame.screen, Screen::Scanner);
        assert_eq!(frame.selected_capture_mode(), 2);
        assert!(
            frame.captures.len() == captures,
            "mode gestures leave the capture resource unchanged"
        );
    }

    #[test]
    fn sample_recording_advances_once_per_update_and_never_during_sync() {
        let (mut app, mut doc, root) = sample_app();
        app.apply(
            Action::SetMode(CaptureMode::Video),
            &mut Edits::lend(&mut doc),
            &root,
        );
        app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
        assert!(app.is_recording());
        let before = app.frame_gen;

        Widget::update(&mut app, &doc, &root);
        assert_eq!(app.frame_gen, before + 1);
        for _ in 0..3 {
            Widget::sync(&mut app, &doc, &root);
            Widget::settle(&mut app, &mut Edits::lend(&mut doc), &root);
        }
        assert_eq!(
            app.frame_gen,
            before + 1,
            "reading and settling a frame cannot feed the recorder twice"
        );
        Widget::update(&mut app, &doc, &root);
        assert_eq!(
            app.frame_gen,
            before + 2,
            "the next update feeds exactly one more frame"
        );

        app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
        sync_until(&mut app, &mut doc, &root, |app| !app.video_finalizing);
        assert!(app.captures.iter().any(Capture::is_video));
    }

    /// Viewfinder washes and HUD ink are chrome on media: they come from the
    /// theme scrim, and the ink clears body-text contrast on it in every
    /// shipped preset (including high contrast).
    #[test]
    fn viewfinder_chrome_follows_the_theme() {
        use ui::a11y::contrast_ratio;
        use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let scrim = theme.tokens.colors.media.scrim;
            let ink = theme.tokens.colors.media.ink;
            assert!(
                contrast_ratio(ink, scrim) >= ON_FILL_MIN_CONTRAST,
                "{name} HUD ink {:.2}:1 on scrim",
                contrast_ratio(ink, scrim)
            );
        }
    }

    /// The sample camera over a record holding the screenshots' recent
    /// capture.
    fn sample_app() -> (CameraApp, AutoCommit, ObjId) {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        ui::framework::Dispatcher::<Camera>::detached("so.tilo.camera", "test-author")
            .submit_now(
                &mut Edits::lend(&mut doc),
                Camera::REGISTER_CAPTURE.with(crate::resource::sample_capture()),
            )
            .unwrap();
        let root = automerge::ROOT;
        app.sync_record(&mut doc, &root);
        (app, doc, root)
    }

    /// Hand `app` the record as the framework presents it.
    fn present(app: &mut CameraApp, doc: &AutoCommit) {
        let state = <Camera as ui::tilo_resource::Resource>::read(doc).unwrap();
        app.present_record(&state);
    }

    impl CameraApp {
        /// One frame as the framework runs it: the surface syncs, then is
        /// handed the record its actions left.
        fn sync_record(&mut self, doc: &mut AutoCommit, root: &ObjId) {
            Widget::update(self, doc, root);
            Widget::settle(self, &mut Edits::lend(doc), root);
            Widget::sync(self, doc, root);
            Widget::settle(self, &mut Edits::lend(doc), root);
            present(self, doc);
        }
    }

    /// The camera record holds the recent capture and the scan history
    /// alone: the mode, the lens and the capture preferences are never
    /// written into it.
    #[test]
    fn the_camera_record_holds_no_preferences() {
        use automerge::ReadDoc as _;
        let (app, doc, root) = sample_app();
        let keys: Vec<String> = doc.keys(&root).collect();
        assert!(
            keys.iter().all(|key| key == "recent" || key == "scans"),
            "{keys:?}"
        );
        assert!(keys.iter().any(|key| key == "recent"));
        assert_eq!(app.mode, CaptureMode::Photo);
        assert_eq!(app.facing, Facing::Back);
        assert_eq!(app.settings, Settings::default());
    }

    fn sync_until(
        app: &mut CameraApp,
        doc: &mut AutoCommit,
        root: &ObjId,
        mut done: impl FnMut(&CameraApp) -> bool,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(app) && ui::time::now() < deadline {
            app.sync_record(doc, root);
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
        ui::testing::Dispatch::new()
            .doc(doc)
            .path(root)
            .send(app, ev)
            .handled
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
            let recent = layout.recent.expect("the recent scans show at every size");
            assert!(
                recent.intersect(layout.guide).area() <= 0.0
                    && recent.intersect(card).area() <= 0.0,
                "{w}x{h}: the recent scans {recent:?} overlap the guide or the card"
            );
            assert!(
                layout.hint.y1 <= layout.guide.y0,
                "{w}x{h}: the detected tag {:?} sits above the guide {:?}",
                layout.hint,
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

    #[test]
    fn the_settings_sheet_opens_over_the_viewfinder_and_applies_changes() {
        // Phase `sheet_is_a_modal_over_the_viewfinder`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(
                Action::Nav(Screen::Settings),
                &mut Edits::lend(&mut doc),
                &root,
            );
            let mut app = render_at(app, doc, 800.0, 600.0);
            let sheet = app.settings_sheet.dialog.dialog_rect();
            assert!(sheet.y0 >= 0.0 && sheet.y1 <= 600.0);
            let center = sheet.center();
            Framed::harness_mut(&mut app).dispatch_to_root(&WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
            });
            Framed::harness_mut(&mut app).dispatch_to_root(&WindowEvent::MouseWheel {
                device_id: winit::event::DeviceId::dummy(),
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, -10.0),
                phase: winit::event::TouchPhase::Moved,
            });
            assert!(app.settings_sheet.dialog.scroll_offset() > 0.0);
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
                &mut Edits::lend(&mut state.doc),
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
                &mut Edits::lend(&mut state.doc),
                &root,
            ));
            camera.sync_record(&mut state.doc, &root);
            assert_eq!(camera.captures.len(), 1);
            assert_ne!(camera.captures[0].id, before);
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
            app.sync_record(&mut doc, &root);
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
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&doc, &root);
            app.camera_issue = Some(CameraIssue::NoDevice);
            let mut app = render_at(app, doc, 390.0, 844.0);
            let (camera, state) = Framed::split(&mut app);
            assert!(camera.perform_semantic_action(
                "camera.retry-camera",
                &A11yAction::Activate,
                &mut Edits::lend(&mut state.doc),
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
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
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
            app.apply(
                Action::SetMode(CaptureMode::Video),
                &mut Edits::lend(&mut doc),
                &root,
            );
            assert!(!app.feed_active());
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
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
            app.apply(
                Action::SetTimer(Timer::S3),
                &mut Edits::lend(&mut doc),
                &root,
            );
            let before = app.captures.len();
            let before_id = app.captures.first().map(|capture| capture.id.clone());
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            assert!(app.is_counting_down());
            assert_eq!(app.captures.len(), before, "timer does not capture early");
            app.countdown.as_mut().expect("armed timer").due = ui::time::now();
            app.sync_record(&mut doc, &root);
            assert!(!app.is_counting_down());
            assert_eq!(app.captures.len(), 1);
            assert_ne!(
                app.captures.first().map(|capture| capture.id.clone()),
                before_id
            );

            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            assert!(app.is_counting_down());
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            assert!(!app.is_counting_down(), "second shutter cancels safely");
        }
        // Phase `sample_shutter_replaces_the_recent_handoff`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = app.captures[0].id.clone();
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            app.sync_record(&mut doc, &root);
            assert_eq!(app.captures.len(), 1);
            assert_ne!(app.captures[0].id, before);
            assert!(app.toast.is_some());
        }
        // Phase `disk_shutter_writes_a_real_jpeg_via_handle_event`.
        {
            use winit::event::{ElementState, WindowEvent};
            use winit::keyboard::{Key, NamedKey};

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

            let ev = ui::synthetic_input::key_event(
                Key::Named(NamedKey::Space),
                None,
                ElementState::Pressed,
            );
            let event = WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event: ev,
                is_synthetic: false,
            };
            let handled = drive(&mut app, &event, &mut doc, &root);
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
                app.sync_record(&mut doc, &root);
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
        // Phase `set_mode_locks_while_recording`.
        {
            let (mut app, mut doc, root) = sample_app();
            assert_eq!(app.mode, CaptureMode::Photo);
            app.apply(
                Action::SetMode(CaptureMode::Video),
                &mut Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.mode, CaptureMode::Video);

            // Shutter in video mode starts recording; mode can't be flipped while
            // a clip is in progress.
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            assert!(app.is_recording());
            app.apply(
                Action::SetMode(CaptureMode::Photo),
                &mut Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.mode, CaptureMode::Video, "mode locked while recording");
        }
        // Phase `recording_requests_frames_and_reduced_motion_uses_a_wake_deadline`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(
                Action::SetMode(CaptureMode::Video),
                &mut Edits::lend(&mut doc),
                &root,
            );
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            assert!(Widget::needs_animation_frame(&app));
            app.set_motion_reduced(true);
            assert!(!Widget::needs_animation_frame(&app));
            assert!(Widget::next_wake_deadline(&app).is_some());
        }
        // Phase `sample_video_record_replaces_the_recent_handoff`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(
                Action::SetMode(CaptureMode::Video),
                &mut Edits::lend(&mut doc),
                &root,
            );

            // Start; the hermetic frame-synth runs on each sync tick.
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
            assert!(app.is_recording());
            for _ in 0..6 {
                app.sync_record(&mut doc, &root);
            }
            assert!(
                app.frame_gen >= 6,
                "sample frames were admitted to the worker"
            );

            // Stop queues finalization; the bounded worker produces a real clip
            // before sync registers its canonical capture resource.
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root);
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
    }

    #[test]
    fn disk_video_recording_writes_a_real_avi() {
        {
            let dir =
                std::env::temp_dir().join(format!("camera-vidrec-test-{}", std::process::id()));
            let mut app = CameraApp::with(Storage::Disk(dir.clone()), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync_record(&mut doc, &root);
            app.apply(
                Action::SetMode(CaptureMode::Video),
                &mut Edits::lend(&mut doc),
                &root,
            );

            // A frame arrives, then record: start seeds with it, further frames are
            // fed straight into the encoder.
            app.push_camera_frame(frame::viewfinder(0, 128, 96));
            assert!(app.feed_active());
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root); // start
            assert!(app.is_recording());
            for i in 1..6u8 {
                app.push_camera_frame(frame::viewfinder(i % 6, 128, 96));
            }
            app.apply(Action::Shutter, &mut Edits::lend(&mut doc), &root); // stop → write clip
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

    thread_local! {
        /// Where [`DiskCamera`] saves its captures.
        static CAPTURE_DIR: std::cell::RefCell<Option<PathBuf>> =
            const { std::cell::RefCell::new(None) };
    }

    /// A live window whose captures go to [`CAPTURE_DIR`], fed the frames a
    /// test pushes instead of a camera's.
    struct DiskCamera;

    impl ui::framework::ResourceApp for DiskCamera {
        type Resource = Camera;
        const ID: &'static str = "so.tilo.camera";
        const TITLE: &'static str = "Camera";

        fn view(cx: &mut ui::framework::ViewCx<Camera>) -> ui::widget::Element {
            let dir = CAPTURE_DIR.with(|dir| dir.borrow().clone()).unwrap();
            cx.surface(CameraApp::with(Storage::Disk(dir), Screen::Camera))
        }
    }

    /// A live window over the sample camera and document scanner: effects
    /// run, no device is touched.
    struct SampleCamera;

    impl ui::framework::ResourceApp for SampleCamera {
        type Resource = Camera;
        const ID: &'static str = "so.tilo.camera";
        const TITLE: &'static str = "Camera";

        fn view(cx: &mut ui::framework::ViewCx<Camera>) -> ui::widget::Element {
            cx.surface(CameraApp::sample())
        }
    }

    /// A live [`DiskCamera`] saving into the fresh directory `name`.
    fn disk_camera(name: &str) -> (ui::framework::testing::TestApp<DiskCamera>, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "camera-{name}-{}-{}",
            std::process::id(),
            unix_now_i64()
        ));
        std::fs::remove_dir_all(&dir).ok();
        CAPTURE_DIR.with(|slot| *slot.borrow_mut() = Some(dir.clone()));
        (
            ui::framework::testing::TestApp::<DiskCamera>::new().unwrap(),
            dir,
        )
    }

    /// Feed `app` the viewfinder frame `seed`.
    fn push_frame(app: &mut ui::framework::testing::TestApp<DiskCamera>, seed: u8) {
        assert_eq!(
            app.with_surface::<CameraApp, _>(|camera| {
                camera.push_camera_frame(frame::viewfinder(seed, 128, 96))
            }),
            Some(true)
        );
    }

    /// A photo whose caller cancelled it while it was being saved is
    /// dropped when it arrives, with its file: nothing is registered.
    #[test]
    fn a_cancelled_capture_is_discarded_with_its_file() {
        let (mut app, dir) = disk_camera("photo-cancelled");
        push_frame(&mut app, 1);
        let invocation = app
            .start_effect_on(crate::actions::CAPTURE_ROW, &CameraEffect::Capture)
            .expect("the photo is being saved");
        assert!(matches!(
            app.cancel_effect(invocation),
            Some(ui::tilo_actions::InvocationUpdate::Cancelled { .. })
        ));
        app.harness()
            .wait_until(ui::time::Duration::from_secs(2), |_| false);
        assert!(app.state().recent.is_none(), "nothing is registered");
        let photos =
            std::fs::read_dir(&dir).map_or(0, |entries| entries.filter_map(Result::ok).count());
        assert_eq!(photos, 0, "the saved file is removed");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn capture_effects_answer_once_the_capture_is_registered() {
        // Phase `a_photo_answers_with_the_registered_capture`.
        {
            let (mut app, dir) = disk_camera("photo-effect");
            push_frame(&mut app, 1);
            let output = app
                .effect_on(crate::actions::CAPTURE_ROW, &CameraEffect::Capture)
                .expect("the photo is saved");
            let value = output.value.expect("the capture's identity");
            let capture_id = value["capture_id"].as_str().unwrap().to_owned();
            assert!(
                value["media_id"]
                    .as_str()
                    .is_some_and(crate::media::is_media_id)
            );
            assert_eq!(output.resources, [format!("camera.photo:{capture_id}")]);
            let recent = app.state().recent.expect("the photo is the recent capture");
            assert_eq!((recent.id, recent.kind), (capture_id, CaptureKind::Photo));
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `a_clip_answers_once_it_is_saved_and_registered`.
        {
            let (mut app, dir) = disk_camera("video-effect");
            app.effect(&CameraEffect::UseMode {
                mode: "video".into(),
            })
            .unwrap();
            push_frame(&mut app, 1);
            assert!(
                app.effect_on(crate::actions::CAPTURE_ROW, &CameraEffect::Capture)
                    .is_err(),
                "a photo needs photo mode"
            );
            app.effect_on(crate::actions::CAPTURE_ROW, &CameraEffect::Record)
                .expect("recording starts");
            for seed in 2..8 {
                push_frame(&mut app, seed);
            }
            let row = app
                .harness()
                .resources()
                .iter()
                .flat_map(|root| root.children.clone())
                .find(|row| row.id.as_str() == crate::actions::CAPTURE_ROW)
                .expect("the capture row");
            assert!(
                row.actions
                    .iter()
                    .any(|action| action.id == "record" && action.label == "Stop recording"),
                "the recorder offers stopping while it records"
            );
            let output = app
                .effect_on(crate::actions::CAPTURE_ROW, &CameraEffect::Record)
                .expect("the clip is saved");
            let capture_id = output.value.unwrap()["capture_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let recent = app.state().recent.expect("the clip is the recent capture");
            assert_eq!((recent.id, recent.kind), (capture_id, CaptureKind::Video));
            assert!(recent.dur_ms > 0);
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `sample_document_capture_answers_after_adding_the_page`.
        {
            let mut app = ui::framework::testing::TestApp::<SampleCamera>::new().unwrap();
            app.effect_on(crate::actions::DOCUMENT_ROW, &CameraEffect::CapturePage)
                .expect("the sample scanner captures a page");
            assert_eq!(
                app.with_surface::<CameraApp, _>(|camera| camera.document_scan.pages.len()),
                Some(1)
            );
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
            app.apply(
                Action::Nav(Screen::Document),
                &mut Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.screen, Screen::Document);
            assert!(app.document_scan.pages.is_empty());

            app.apply(Action::CaptureDocument, &mut Edits::lend(&mut doc), &root);
            assert_eq!(app.document_scan.pages.len(), 1);
            let page = app.document_scan.pages[0].rendered();
            assert_eq!((page.width(), page.height()), (1240, 1754));
            assert_eq!(app.toast.as_deref(), Some("Page 1 captured"));

            app.apply(Action::BeginDocumentCrop, &mut Edits::lend(&mut doc), &root);
            assert!(app.document_crop_editor.is_some());
            app.document_crop_selection.set(CropSelection {
                left: 0.1,
                top: 0.1,
                right: 0.9,
                bottom: 0.9,
            });
            app.apply(Action::ApplyDocumentCrop, &mut Edits::lend(&mut doc), &root);
            assert!(app.document_crop_editor.is_none());
            let page = app.document_scan.pages[0].rendered();
            assert_eq!((page.width(), page.height()), (992, 1404));

            app.apply(Action::RotateDocument, &mut Edits::lend(&mut doc), &root);
            let page = app.document_scan.pages[0].rendered();
            assert_eq!((page.width(), page.height()), (1404, 992));

            let (sender, receiver) = mpsc::channel();
            app.document_job = Some(receiver);
            app.document_job_kind = Some(DocumentJobKind::Recognize);
            app.apply(
                Action::RemoveDocumentPage,
                &mut Edits::lend(&mut doc),
                &root,
            );
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

            app.apply(
                Action::RemoveDocumentPage,
                &mut Edits::lend(&mut doc),
                &root,
            );
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
            app.apply(Action::CaptureDocument, &mut Edits::lend(&mut doc), &root);
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
            app.apply(Action::CopyDocumentText, &mut Edits::lend(&mut doc), &root);
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
                app.apply(
                    Action::SetDocumentChoice(choice, selection),
                    &mut Edits::lend(&mut doc),
                    &root,
                );
            }
            app.drain_ui_queue(&mut Edits::lend(&mut doc), &root);

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
            app.apply(
                Action::Nav(Screen::Document),
                &mut Edits::lend(&mut doc),
                &root,
            );
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
                    device_id: winit::event::DeviceId::dummy(),
                    phase,
                    location: winit::dpi::PhysicalPosition::new(point.x, point.y),
                    force: None,
                    id: 17,
                })
            }

            let (mut app, mut doc, root) = sample_app();
            app.apply(
                Action::Nav(Screen::Document),
                &mut Edits::lend(&mut doc),
                &root,
            );
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
    fn capture_preferences_and_screens_change_no_record() {
        // Phase `screen_reveals_are_presentation_only`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = doc.get_heads();
            app.apply(
                Action::Nav(Screen::Settings),
                &mut Edits::lend(&mut doc),
                &root,
            );
            assert_eq!(app.screen, Screen::Settings);
            assert_eq!(doc.get_heads(), before);
        }
        // Phase `cycle_facing`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = doc.get_heads();
            assert_eq!(app.facing, Facing::Back);
            app.apply(Action::CycleFacing, &mut Edits::lend(&mut doc), &root);
            assert_eq!(app.facing, Facing::Front);
            assert_eq!(doc.get_heads(), before);
        }
        // Phase `toggle_grid`.
        {
            let (mut app, mut doc, root) = sample_app();
            let before = app.settings.grid;
            let heads = doc.get_heads();
            app.apply(Action::ToggleGrid, &mut Edits::lend(&mut doc), &root);
            assert_eq!(app.settings.grid, !before);
            assert_eq!(doc.get_heads(), heads);
        }
        // Phase `resolution_cycles`.
        {
            let (mut app, mut doc, root) = sample_app();
            assert_eq!(app.settings.resolution, Resolution::R12MP);
            app.apply(Action::CycleResolution, &mut Edits::lend(&mut doc), &root);
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
            app.sync_record(&mut doc, &root); // decodes the sample poster → scanned set
            // Force the scanner to decode by rendering once would need a GPU; the
            // sample scanner frame is produced lazily on draw, so seed a scanned
            // code directly to exercise the action path.
            app.scanned = Some(ScannedCode {
                raw: SCAN_PAYLOAD.to_string(),
                kind: classify(SCAN_PAYLOAD),
                symbology: "QR Code".into(),
                corners: [(0.0, 0.0); 4],
            });
            app.apply(Action::Scan(0), &mut Edits::lend(&mut doc), &root);
            let scans = <crate::record::Record as ui::tilo_resource::Resource>::read(&doc)
                .unwrap()
                .scans;
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
            app.sync_record(&mut doc, &root);
            app.scanned = Some(ScannedCode {
                raw: payload.to_string(),
                kind,
                symbology: "QR Code".into(),
                corners: [(0.0, 0.0); 4],
            });
            app.apply(Action::Scan(0), &mut Edits::lend(&mut doc), &root); // "Connect"
            assert_eq!(app.toast.as_deref(), Some("Wi-Fi password copied"));
        }
    }

    /// A real QR code rasterized losslessly as PNG bytes.
    fn qr_png(payload: &str) -> Vec<u8> {
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
        library::encode_photo(&fb, PhotoFormat::Png).unwrap().1
    }

    /// Scanning an existing image and exporting scanned pages go through the
    /// host's file chooser, from the palette, an agent or the screen alike.
    #[test]
    fn the_file_chooser_feeds_the_code_scanner_and_takes_the_document_export() {
        let dir = std::env::temp_dir().join(format!(
            "camera-chooser-{}-{}",
            std::process::id(),
            unix_now_i64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = ui::framework::testing::TestApp::<SampleCamera>::new().unwrap();

        // Phase `a_chosen_image_decodes_a_real_qr_into_the_history`.
        let payload = "https://example.test/scanned-from-a-saved-photo";
        let image = dir.join("code.png");
        std::fs::write(&image, qr_png(payload)).unwrap();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(image));
        app.effect_on(crate::actions::SCANNER_ROW, &CameraEffect::ImportImage)
            .expect("the chosen image decodes");
        assert_eq!(
            recorder.pick_requests()[0].title(),
            "Scan a code from an image"
        );
        let scanned = app
            .with_surface::<CameraApp, _>(|camera| camera.scanned.clone())
            .flatten()
            .expect("the scanned code is shown");
        assert_eq!(scanned.raw, payload);
        // The decode is recorded to history like a live scan.
        let scans = app.state().scans;
        assert_eq!(scans.len(), 1);
        assert_eq!(scans[0].raw, payload);

        // Phase `a_cancelled_chooser_answers_and_closes`.
        recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
        let cancelled = app
            .effect_on(crate::actions::SCANNER_ROW, &CameraEffect::ImportImage)
            .unwrap_err();
        assert!(cancelled.to_string().contains("cancelled"), "{cancelled}");
        app.settle();
        assert_eq!(
            app.with_surface::<CameraApp, _>(|camera| camera.scan_import_pending.get()),
            Some(false),
            "the chooser is no longer shown as open"
        );
        assert_eq!(app.state().scans.len(), 1);

        // Phase `scanned_pages_export_as_a_searchable_pdf`.
        assert!(
            app.effect_on(crate::actions::DOCUMENT_ROW, &CameraEffect::ExportPdf)
                .is_err(),
            "there is nothing to export before a page is captured"
        );
        app.effect_on(crate::actions::DOCUMENT_ROW, &CameraEffect::CapturePage)
            .unwrap();
        let pdf = dir.join("scan.pdf");
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(pdf.clone()));
        app.effect_on(crate::actions::DOCUMENT_ROW, &CameraEffect::ExportPdf)
            .expect("the document is saved");
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The camera publishes its screens as views, and the capture pipeline,
    /// the code scanner and the document scanner as rows offering only
    /// effects bound to them.
    #[test]
    fn the_rows_offer_the_camera_effects_on_what_they_act_on() {
        let mut app = ui::framework::testing::TestApp::<SampleCamera>::new().unwrap();
        let resources = app.harness().resources();
        let camera = resources
            .iter()
            .find(|resource| resource.id.as_str() == "camera")
            .expect("the camera");
        let ids = |resource: &ui::tilo_actions::Resource| {
            resource
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect::<Vec<_>>()
        };
        let camera_ids = ids(camera);
        for id in [
            "open-camera",
            "open-scanner",
            "open-document",
            "open-settings",
            "use-mode",
            "use-lens",
            "set-grid",
        ] {
            assert!(
                camera_ids.contains(&id.to_owned()),
                "{id} in {camera_ids:?}"
            );
        }
        let row = |id: &str| {
            camera
                .children
                .iter()
                .find(|row| row.id.as_str() == id)
                .unwrap_or_else(|| panic!("{id} is a row"))
        };
        assert_eq!(
            ids(row(crate::actions::CAPTURE_ROW)),
            ["open-photos", "capture", "record"]
        );
        assert_eq!(ids(row(crate::actions::SCANNER_ROW)), ["import-image"]);
        assert_eq!(
            ids(row(crate::actions::DOCUMENT_ROW)),
            [
                "capture-page",
                "rotate-page",
                "recognize-text",
                "copy-text",
                "export-pdf",
                "crop-page",
                "move-page-earlier",
                "move-page-later",
                "remove-page",
            ]
        );

        app.effect(&CameraEffect::OpenSettings).unwrap();
        assert_eq!(
            app.with_surface::<CameraApp, _>(|camera| camera.screen),
            Some(Screen::Settings)
        );
        assert!(
            app.effect_on(crate::actions::DOCUMENT_ROW, &CameraEffect::RotatePage)
                .is_err(),
            "a page operation waits for a page"
        );
    }
}
