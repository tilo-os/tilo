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
use tilo_clock::unix_now_i64;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect};
use vello::peniko::Color;

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::widget::ActionEmitter;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, CropSelection, Image, ImageCropEditor, ImageFit, Popover, Toggle,
    ToggleFace, Widget,
};
use winit::event::WindowEvent;

use crate::doc;
use crate::frame::{self, FrameBuf};
use crate::library;
use crate::model::{
    Capture, CaptureKind, CaptureMode, CodeKind, Facing, PhotoFormat, Resolution, ScanEntry,
    ScannedCode, Screen, Settings, Timer, classify,
};
use crate::paint as p;

/// Uniform inset for floating controls from the window edge. The camera is an
/// immersive, full-bleed app: the viewfinder fills the window edge-to-edge and
/// every control floats over it at this margin — there is no navigation rail
/// or sidebar (the OS owns window chrome). Gallery and Settings are reached
/// from the viewfinder as a push and an overlay sheet, not as rail items.
/// A `Spacing::BASE` step as the `f64` this painter's geometry uses.
#[allow(
    clippy::cast_lossless,
    reason = "const context: `f64::from` is not const; f32 -> f64 is exact"
)]
const fn step(px: f32) -> f64 {
    px as f64
}
/// The over-media chrome's inset from the window edge: section padding
/// (HIG §7b `s4`).
const EDGE: f64 = step(Spacing::BASE.s4);
/// Siblings beside each other (setting pills, dialog buttons): `s2` on the
/// dense viewfinder strip, `s3` between a dialog's two actions.
const PILL_GAP: f64 = step(Spacing::BASE.s2);
const ACTION_GAP: f64 = step(Spacing::BASE.s3);
/// Separators inside a grouped card are inset `s3` from both ends (HIG §9.5).
const SEPARATOR_INSET: f64 = step(Spacing::BASE.s3);
/// Floating over-viewfinder capsules (the top setting pills and the
/// Photo/Video/Scan switcher) are authored at these 1× heights and grow only
/// when the measured label line no longer fits — so 200% text is contained
/// without changing the 1× viewfinder composition. Label sizes stay literal:
/// these are over-media overlays (see `docs/hand-rolled-ui-audit.md`), drawn
/// dark-on-light regardless of theme, and the renderer applies the text scale.
const CTRL_PILL_MIN_H: f64 = 30.0;
const CTRL_PILL_LABEL_SIZE: f32 = 12.0;
const CTRL_PILL_PAD_Y: f64 = 7.5;
const MODE_SWITCHER_MIN_H: f64 = 34.0;
const MODE_LABEL_SIZE: f32 = 13.0;
const MODE_LABEL_PAD_Y: f64 = step(Spacing::BASE.s2);
/// Horizontal room either side of a segment label. Segments split their
/// capsule equally, so the widest measured label sets the capsule's floor
/// (`segmented_min_w`); the authored 1× widths are wider than that floor and
/// stay in charge until an accessibility text scale needs more.
const MODE_LABEL_PAD_X: f64 = step(Spacing::BASE.s3);
/// Over-media badges (detected-code tag, gallery duration/"Unavailable" tags)
/// keep their authored 1× heights and grow to `label line + 2 × BADGE_PAD_Y`
/// once the measured line no longer fits.
const BADGE_PAD_Y: f64 = 3.0;
/// Recent-scans rows: authored 44 px, measured summary + timestamp block.
const SCAN_ROW_PAD_Y: f64 = 6.0;
/// Payload composited into the sample scanner poster (hermetic path only).
const SCAN_PAYLOAD: &str = "WIFI:T:WPA2;S:Tilo Lounge;P:welcome-aboard;H:false;;";
/// Single decoded-size budget for the contextual recent-capture preview.
const THUMB: (u32, u32) = (512, 342);
/// Below this content width the document scanner folds to a single column.
const WIDE_MIN: f64 = 900.0;
/// Renderer/upload admission limit for external camera data. This mirrors the
/// capture callback limit and remains here for the feature-off/test path: no
/// caller can smuggle malformed pixels into an `Image`.
const MAX_FRAME_PIXELS: u64 = 8_847_360; // 4096 × 2160
const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;
/// Keep only the current and immediately superseded thumbnail while an async
/// recent-capture handoff changes; Camera never owns a browsing cache.
const MAX_THUMB_CACHE: usize = 2;
/// Ink for chrome sitting on the viewfinder. Measured against the theme's
/// scrim so high contrast and a light desktop both stay readable on the
/// dark wash over the feed.
fn hud_ink(theme: &Theme) -> Color {
    theme.on_surface(theme.tokens.colors.surfaces.scrim).ink
}

fn media_scrim(theme: &Theme) -> Color {
    theme.tokens.colors.surfaces.scrim
}
fn shrink(r: Rect, d: f64) -> Rect {
    Rect::new(r.x0 + d, r.y0 + d, r.x1 - d, r.y1 - d)
}

const fn scan_color_label(color: tilo_capture::scanner::ScanColor) -> &'static str {
    match color {
        tilo_capture::scanner::ScanColor::Color => "Color",
        tilo_capture::scanner::ScanColor::Grayscale => "Grayscale",
        tilo_capture::scanner::ScanColor::LineArt => "Black & white",
    }
}

const fn scan_source_label(source: tilo_capture::scanner::ScanSource) -> &'static str {
    match source {
        tilo_capture::scanner::ScanSource::Flatbed => "Flatbed",
        tilo_capture::scanner::ScanSource::Feeder => "Feeder",
        tilo_capture::scanner::ScanSource::FeederDuplex => "Duplex feeder",
    }
}

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
    const fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::German => "German",
            Self::French => "French",
            Self::Spanish => "Spanish",
        }
    }

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
    OpenDocumentChoice(DocumentChoice),
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
    CycleFormat,
    SetTimer(Timer),
    ToggleGrid,
    ToggleLevel,
    ToggleMirror,
    ToggleGeotag,
    ToggleHdr,
    CycleDefaultFacing,
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
    last_mouse: Point,
    settings_scroll: f64,
    document_scan: crate::document_scan::ScanSession,
    document_settings: DocumentSettings,
    document_choice: Option<DocumentChoice>,
    document_choice_anchors: [Rect; 5],
    document_popover: Popover,
    scanner_devices: Vec<tilo_capture::scanner::Scanner>,
    selected_scanner: Option<String>,
    document_save_picker: Option<ui::portal::PortalChannel>,
    scan_image_picker: Option<ui::portal::PortalChannel>,
    document_crop_selection: ui::LocalState<CropSelection>,
    document_crop_editor: Option<ImageCropEditor>,
    document_crop_page: Option<usize>,
    document_preview: Option<(
        usize,
        crate::document_scan::Crop,
        crate::document_scan::Rotation,
        Image,
    )>,
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
    camera_chooser_open: bool,
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
    scan_view: Option<(u32, u32, Image)>,
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
            last_mouse: Point::new(-1_000.0, -1_000.0),
            settings_scroll: 0.0,
            document_scan: crate::document_scan::ScanSession::default(),
            document_settings: DocumentSettings::default(),
            document_choice: None,
            document_choice_anchors: [Rect::ZERO; 5],
            document_popover: Popover::new(),
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
            camera_chooser_open: false,
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

    /// Register a compact painted affordance with a 44 pt minimum touch and
    /// accessibility target. Larger cards keep their exact bounds through
    /// [`Self::push_region`] so neighbouring capture controls never overlap.
    fn push_tap_region(&mut self, rect: Rect, action: Action) {
        let w = rect.width().max(44.0);
        let h = rect.height().max(44.0);
        let expanded = Rect::new(
            rect.center().x - w / 2.0,
            rect.center().y - h / 2.0,
            rect.center().x + w / 2.0,
            rect.center().y + h / 2.0,
        );
        let clamped = Rect::new(
            expanded.x0.max(self.rect.x0),
            expanded.y0.max(self.rect.y0),
            expanded.x1.min(self.rect.x1),
            expanded.y1.min(self.rect.y1),
        );
        self.push_region(clamped, action);
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
            Action::OpenDocumentChoice(choice) => {
                format!("camera.document.choice:{}", choice.index())
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
            Action::CycleFormat => "camera.format".into(),
            Action::SetTimer(timer) => format!("camera.timer:{}", timer.seconds()),
            Action::ToggleGrid => "camera.grid".into(),
            Action::ToggleLevel => "camera.level".into(),
            Action::ToggleMirror => "camera.mirror".into(),
            Action::ToggleGeotag => "camera.geotag".into(),
            Action::ToggleHdr => "camera.hdr".into(),
            Action::CycleDefaultFacing => "camera.default-facing".into(),
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
            Action::CycleFormat => (
                Role::Button,
                format!(
                    "Change photo format, currently {}",
                    self.settings.photo_format.label()
                ),
                AccessState::new().focusable(true),
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
            Action::CycleDefaultFacing => (
                Role::Button,
                format!(
                    "Change default camera, currently {}",
                    self.settings.default_facing.label()
                ),
                AccessState::new().focusable(true),
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
            Action::OpenDocumentChoice(choice) => {
                let (setting, value) = match choice {
                    DocumentChoice::Scanner => (
                        "scanner",
                        self.selected_scanner
                            .as_ref()
                            .and_then(|selected| {
                                self.scanner_devices
                                    .iter()
                                    .find(|scanner| scanner.device == *selected)
                            })
                            .map_or_else(
                                || "none selected".to_owned(),
                                |scanner| scanner.description.clone(),
                            ),
                    ),
                    DocumentChoice::Resolution => (
                        "scan resolution",
                        format!("{} DPI", self.document_settings.scan.resolution_dpi),
                    ),
                    DocumentChoice::Color => (
                        "scan color",
                        scan_color_label(self.document_settings.scan.color).to_owned(),
                    ),
                    DocumentChoice::Source => (
                        "paper source",
                        scan_source_label(self.document_settings.scan.source).to_owned(),
                    ),
                    DocumentChoice::Language => (
                        "text language",
                        self.document_settings.ocr_language.label().to_owned(),
                    ),
                };
                (
                    Role::Button,
                    format!("Choose {setting}, currently {value}"),
                    AccessState::new()
                        .focusable(true)
                        .expanded(
                            self.document_popover.is_open()
                                && self.document_choice == Some(*choice),
                        )
                        .disabled(self.document_job.is_some()),
                )
            }
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
            Action::OpenDocumentChoice(choice) => self.open_document_choice(choice),
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
            | Action::CycleFormat
            | Action::SetTimer(_)
            | Action::ToggleGrid
            | Action::ToggleLevel
            | Action::ToggleMirror
            | Action::ToggleGeotag
            | Action::ToggleHdr
            | Action::CycleDefaultFacing) => {
                self.apply_settings_toggle(&settings_action, doc, root);
            }
        }
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

    fn open_document_choice(&mut self, choice: DocumentChoice) {
        if self.document_job.is_some() {
            return;
        }
        if choice == DocumentChoice::Scanner {
            let selected = self
                .selected_scanner
                .as_ref()
                .and_then(|selected| {
                    self.scanner_devices
                        .iter()
                        .position(|scanner| scanner.device == *selected)
                })
                .unwrap_or(0);
            self.document_choice = Some(choice);
            self.document_popover.set_label("Document scanner");
            self.document_popover.set_items(
                self.scanner_devices
                    .iter()
                    .map(|scanner| scanner.description.clone()),
            );
            self.document_popover.set_viewport(self.rect);
            let anchor = self
                .document_choice_anchors
                .get(choice.index())
                .copied()
                .unwrap_or(Rect::ZERO);
            self.document_popover.open_at(anchor, Some(selected));
            return;
        }
        let (label, items, selected): (&str, &[&str], usize) = match choice {
            DocumentChoice::Scanner => return,
            DocumentChoice::Resolution => (
                "Scan resolution",
                &["150 DPI", "300 DPI", "600 DPI"],
                match self.document_settings.scan.resolution_dpi {
                    150 => 0,
                    600 => 2,
                    _ => 1,
                },
            ),
            DocumentChoice::Color => (
                "Scan color",
                &["Color", "Grayscale", "Black & white"],
                match self.document_settings.scan.color {
                    tilo_capture::scanner::ScanColor::Color => 0,
                    tilo_capture::scanner::ScanColor::Grayscale => 1,
                    tilo_capture::scanner::ScanColor::LineArt => 2,
                },
            ),
            DocumentChoice::Source => (
                "Paper source",
                &["Flatbed", "Feeder", "Duplex feeder"],
                match self.document_settings.scan.source {
                    tilo_capture::scanner::ScanSource::Flatbed => 0,
                    tilo_capture::scanner::ScanSource::Feeder => 1,
                    tilo_capture::scanner::ScanSource::FeederDuplex => 2,
                },
            ),
            DocumentChoice::Language => (
                "Text language",
                &["English", "German", "French", "Spanish"],
                match self.document_settings.ocr_language {
                    OcrLanguage::English => 0,
                    OcrLanguage::German => 1,
                    OcrLanguage::French => 2,
                    OcrLanguage::Spanish => 3,
                },
            ),
        };
        self.document_choice = Some(choice);
        self.document_popover.set_label(label);
        self.document_popover.set_items(items.iter().copied());
        self.document_popover.set_viewport(self.rect);
        let anchor = self
            .document_choice_anchors
            .get(choice.index())
            .copied()
            .unwrap_or(Rect::ZERO);
        self.document_popover.open_at(anchor, Some(selected));
    }

    fn apply_document_choice(&mut self, index: usize) {
        match self.document_choice {
            Some(DocumentChoice::Scanner) => {
                if let Some(scanner) = self.scanner_devices.get(index) {
                    self.selected_scanner = Some(scanner.device.clone());
                    self.toast = Some(format!("Using {}", scanner.description));
                }
            }
            Some(DocumentChoice::Resolution) => {
                if let Some(value) = [150, 300, 600].get(index) {
                    self.document_settings.scan.resolution_dpi = *value;
                }
            }
            Some(DocumentChoice::Color) => {
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
            Some(DocumentChoice::Source) => {
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
            Some(DocumentChoice::Language) => {
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
            None => {}
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
                |angle| format!("{summary} · Straightened {:.1}°", angle.abs()),
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
                                "Recognized {words} words · Straightened {:.1}°",
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
            Action::CycleFormat => {
                self.settings.photo_format = cycle(&PhotoFormat::ALL, self.settings.photo_format);
                crate::actions::CameraAction::SetPhotoFormat {
                    photo_format: self.settings.photo_format,
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
            Action::CycleDefaultFacing => {
                self.settings.default_facing = self.settings.default_facing.next();
                crate::actions::CameraAction::SetDefaultFacing {
                    facing: self.settings.default_facing,
                }
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
        self.camera_chooser_open = false;
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
            self.camera_chooser_open = true;
        } else {
            self.toast = Some("Only one camera is available".into());
        }
    }

    fn select_camera(&mut self, device: &str) {
        // Revalidate exactly when an agent, semantic action, or pointer asks
        // to select: a stale /dev/video node must not become a pretend lens.
        if !self.available_camera_devices().iter().any(|d| d == device) {
            self.camera_chooser_open = false;
            self.toast = Some("That camera is no longer available".into());
            return;
        }
        self.selected_camera = Some(device.to_string());
        self.camera_chooser_open = false;
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
    /// handed to the OS default handler; everything is also copied to the
    /// clipboard so the action does something real on any host.
    fn perform_scan_action(&mut self, s: &ScannedCode, i: usize) {
        let actions = s.kind.actions();
        let label = actions.get(i).copied().unwrap_or("Copy");
        match label {
            "Open link" | "Open map" | "Compose" | "Call" => {
                ui::widget::open_external_url(&s.raw);
                ui::clipboard_copy(&s.raw);
                self.toast = Some(format!("{label} · copied to clipboard"));
            }
            "Connect" | "Copy password" => {
                if let CodeKind::Wifi { password, .. } = &s.kind {
                    ui::clipboard_copy(password);
                    self.toast = Some("Wi-Fi password copied".into());
                } else {
                    ui::clipboard_copy(&s.raw);
                    self.toast = Some("Copied to clipboard".into());
                }
            }
            _ => {
                ui::clipboard_copy(&s.raw);
                self.toast = Some("Copied to clipboard".into());
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

fn now_secs() -> i64 {
    if let Ok(value) = std::env::var("CAMERA_SHOT_NOW")
        && let Ok(now) = value.parse::<i64>()
    {
        return now.max(0);
    }
    unix_now_i64()
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
            if std::env::var_os("CAMERA_SHOT_TIMER_RUNNING").is_some() && self.countdown.is_none() {
                self.settings.timer = Timer::S3;
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
        // Screenshot hook: pin the Settings scroll position for headless rendering.
        if let Ok(v) = std::env::var("CAMERA_SCROLL")
            && let Ok(px) = v.parse::<f64>()
        {
            self.settings_scroll = px;
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
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
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
        p::fill_rect(scene, t, self.rect, theme.tokens.colors.surfaces.base);
        // Camera is an immersive capture surface.  Nothing may reserve a rail
        // or drawer width from its viewfinder: secondary tools are reached by
        // the compact mode switcher and settings is a sheet over this surface.
        let full = self.rect;
        match self.screen {
            Screen::Camera => self.draw_camera(scene, t, tr, theme, full),
            Screen::Scanner => self.draw_scanner(scene, t, tr, theme, full),
            Screen::Document => self.draw_document_scanner(scene, t, tr, theme, full),
            Screen::Settings => self.draw_settings(scene, t, tr, theme, full),
        }
        self.draw_hover_tooltip(scene, t, tr, theme);
        self.draw_toast(scene, t, tr, theme);
        self.document_popover.draw(scene, t, tr, theme, presence);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the root event router keeps pointer, touch, keyboard, crop, modal, and drawer precedence together so painted and semantic hit-testing cannot diverge"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase};
        use winit::keyboard::{Key, NamedKey};

        if self.document_popover.is_open() {
            self.document_popover.handle_event(event, ctx);
            if let Some(index) = self.document_popover.take_selection() {
                self.apply_document_choice(index);
            }
            self.dirty = true;
            return;
        }
        if let Some(editor) = self.document_crop_editor.as_mut()
            && editor.handle_event_at(event, ctx.scale_factor)
        {
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.hit(self.last_mouse, ctx.state, ctx.path) {
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
                                self.apply(target.action, ctx.state, ctx.path);
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
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / ctx.scale_factor,
                };
                // Wheel up (dy > 0) scrolls toward the top of the content.
                if self.screen == Screen::Settings {
                    self.settings_scroll = (self.settings_scroll - dy).max(0.0);
                    self.dirty = true;
                    *ctx.handled = true;
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
                    Key::Named(NamedKey::Escape) if matches!(self.screen, Screen::Settings) => {
                        Some(Action::Back)
                    }
                    _ => None,
                };
                if let Some(a) = act {
                    self.apply(a, ctx.state, ctx.path);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
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
        self.dirty
    }
    fn needs_animation_frame(&self) -> bool {
        // Recording is an operational frame pump, not a decorative effect:
        // it advances the elapsed indicator and drains the real camera worker.
        // Reduced-motion users receive the same capture correctness through a
        // low-frequency wake deadline below, without a continuous UI loop.
        // A live viewfinder is content, not motion: it runs regardless.
        self.viewfinder_live()
            || ((self.recording.is_some() || self.countdown.is_some()) && !self.motion_reduced)
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.awaiting_camera() {
            // Where access is prompted (the page, macOS) devices appear
            // asynchronously; look again rather than wait for input.
            return Some(ui::time::now() + Duration::from_secs(1));
        }
        if self.motion_reduced && (self.recording.is_some() || self.countdown.is_some()) {
            // The visible counter changes once per second; four wakeups per
            // second keep worker settlement responsive without recreating a
            // 30 fps decorative frame loop for reduced-motion users.
            Some(ui::time::now() + Duration::from_millis(250))
        } else {
            None
        }
    }
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
        self.dirty = true;
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
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

// ====================================================================
// Shared chrome + control helpers
// ====================================================================

/// Icon/title/subtitle/style bundle for [`CameraApp::draw_empty_state`].
#[derive(Clone, Copy)]
struct EmptyStateContent<'a> {
    icon: &'a str,
    title: &'a str,
    subtitle: Option<&'a str>,
    glass: bool,
    recovery: bool,
}

/// Geometry returned by the shared sparse-state renderer so its owner can add
/// real recovery controls without duplicating the card layout.
struct EmptyStateLayout {
    panel: Rect,
    icon: Rect,
}

impl CameraApp {
    /// Horizontal centre of the edge-to-edge stage — the shutter, mode
    /// switcher, toasts and recording badge deliberately share this axis.
    fn stage_center_x(&self) -> f64 {
        self.rect.center().x
    }

    /// A round floating glass button (used for the settings gear, the lens
    /// switch and back buttons over the viewfinder). Returns its hit rect.
    fn glass_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        circle: Circle,
        icon: &str,
        action: Action,
    ) -> Rect {
        let (cx, cy, r) = (circle.center.x, circle.center.y, circle.radius);
        let c = &theme.tokens.colors;
        p::circle(scene, t, cx, cy, r, p::alpha(c.neutral[0], 0.5));
        p::circle_stroke(
            scene,
            t,
            cx,
            cy,
            r,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        let ir = r * 0.52;
        p::icon(
            scene,
            t,
            icon,
            Rect::new(cx - ir, cy - ir, cx + ir, cy + ir),
            hud_ink(theme),
            1.9,
        );
        let hit = Rect::new(cx - r, cy - r, cx + r, cy + r);
        self.push_tap_region(hit, action);
        hit
    }

    /// The Photo / Video / Scan mode switcher that floats near the bottom of
    /// the viewfinder on both the Camera and Scanner screens. Photo and Video
    /// are capture sub-modes on the Camera screen; Scan switches to the
    /// scanner. This is how the viewfinder screens navigate between each other
    /// now that there is no rail.
    fn draw_mode_switcher(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        center_x: f64,
        bottom_y: f64,
    ) {
        // Keep the four capture destinations inside a narrow/high-zoom viewport
        // instead of letting the fixed desktop-width pill clip off-screen.
        let on_cam = matches!(self.screen, Screen::Camera);
        let items: Vec<(String, bool, Action)> = vec![
            (
                "Photo".to_string(),
                on_cam && self.mode == CaptureMode::Photo,
                Action::SetMode(CaptureMode::Photo),
            ),
            (
                "Video".to_string(),
                on_cam && self.mode == CaptureMode::Video,
                Action::SetMode(CaptureMode::Video),
            ),
            (
                "Codes".to_string(),
                matches!(self.screen, Screen::Scanner),
                Action::Nav(Screen::Scanner),
            ),
            (
                "Docs".to_string(),
                matches!(self.screen, Screen::Document),
                Action::Nav(Screen::Document),
            ),
        ];
        // The capsule is anchored by its bottom edge — it sits directly above
        // the shutter — and grows *upward* to contain its labels. At 1× the
        // measured 13 px line fits the authored 34 px, so nothing moves; at an
        // accessibility text scale the labels stop painting below the selected
        // segment's background (corrected-review-1, camera P2). Measured from
        // the labels actually drawn, so a renamed or localized mode still fits.
        let label_h = items
            .iter()
            .map(|(label, _, _)| p::text_h(tr, label, MODE_LABEL_SIZE))
            .fold(0.0_f64, f64::max);
        let seg_h = (label_h + MODE_LABEL_PAD_Y * 2.0)
            .max(MODE_SWITCHER_MIN_H * f64::from(theme.text_scale));
        // Keep the three mode targets inside a narrow/high-zoom viewport
        // instead of letting the fixed desktop-width pill clip off-screen.
        // The authored 150..240 px capsule is the 1× width; at an accessibility
        // text scale the measured labels set the floor (they used to crowd
        // each other in 80 px segments at 200%), capped at the viewport.
        let available = self.rect.width() - EDGE * 2.0;
        let labels: Vec<&str> = items.iter().map(|(label, _, _)| label.as_str()).collect();
        let seg_w = available
            .clamp(150.0, 240.0)
            .max(Self::segmented_min_w(tr, &labels))
            .min(available.max(1.0));
        let seg = Rect::new(
            center_x - seg_w / 2.0,
            bottom_y - seg_h,
            center_x + seg_w / 2.0,
            bottom_y,
        );
        self.segmented(scene, t, tr, theme, seg, items);
    }

    /// The narrowest capsule whose equal segments each hold the widest
    /// measured label with [`MODE_LABEL_PAD_X`] either side. Callers take
    /// the larger of their authored 1× width and this, so nothing moves at
    /// 1× and 200% labels stop running into their neighbours.
    fn segmented_min_w(tr: &mut TextRenderer, labels: &[&str]) -> f64 {
        let widest = labels
            .iter()
            .map(|label| f64::from(p::text_w(tr, label, MODE_LABEL_SIZE)))
            .fold(0.0_f64, f64::max);
        (widest + MODE_LABEL_PAD_X * 2.0) * labels.len().max(1) as f64
    }

    fn segmented(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        items: Vec<(String, bool, Action)>,
    ) {
        let c = &theme.tokens.colors;
        p::fill_round(
            scene,
            t,
            rect,
            rect.height() / 2.0,
            p::alpha(c.neutral[2], 0.92),
        );
        p::stroke_round(
            scene,
            t,
            rect,
            rect.height() / 2.0,
            surface::edge_on(c.neutral[2]),
            1.0,
        );
        let n = items.len().max(1);
        let seg_w = rect.width() / n as f64;
        for (i, (label, selected, action)) in items.into_iter().enumerate() {
            let seg = Rect::new(
                rect.x0 + i as f64 * seg_w,
                rect.y0,
                rect.x0 + (i as f64 + 1.0) * seg_w,
                rect.y1,
            );
            if selected {
                p::fill_round(
                    scene,
                    t,
                    shrink(seg, 3.0),
                    (seg.height() - 6.0) / 2.0,
                    theme.accent_primary,
                );
            }
            let col = if selected {
                theme
                    .tokens
                    .colors
                    .on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
            } else {
                p::alpha(c.text_primary, 0.78)
            };
            p::text_in(scene, tr, seg, &label, MODE_LABEL_SIZE, col);
            self.push_tap_region(seg, action);
        }
    }

    fn toggle_switch(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        rect: Rect,
        on: bool,
        action: Action,
    ) {
        // Shared switch face on the settings sheet (solid chrome, not over-media):
        // themed track/knob/focus plus hover feedback, replacing the hand-painted
        // pill. The existing region hit-test still fires the toggle action.
        let hovered = rect.contains(self.last_mouse);
        Toggle::paint(scene, t, theme, &ToggleFace::new(rect, on).hovered(hovered));
        self.push_tap_region(rect, action);
    }

    /// A centred status card for sparse states such as no available camera:
    /// a bounded, softly-raised panel holding an icon, title and optional
    /// subtitle. Anchoring the message in a real panel — capped in width and
    /// centred in `area` — keeps it from reading as a lone cluster stranded in
    /// a wide, empty window. Returns the icon's rect so callers can decorate it
    /// (e.g. a "no camera" slash).
    fn draw_empty_state(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        content: &EmptyStateContent<'_>,
    ) -> EmptyStateLayout {
        let EmptyStateContent {
            icon,
            title,
            subtitle,
            glass,
            recovery,
        } = *content;
        let c = &theme.tokens.colors;
        // Bounded card, centred in `area`: wide enough to read as a panel,
        // capped so it never stretches edge-to-edge on ultra-wide windows.
        let pw = 360.0_f64.min((area.width() - 48.0).max(240.0));
        let base_h: f64 = if recovery {
            288.0
        } else if subtitle.is_some() {
            208.0
        } else {
            172.0
        };
        let ph = base_h.min((area.height() - 32.0).max(140.0));
        let cx = area.center().x;
        let cy = area.center().y;
        let panel = Rect::new(cx - pw / 2.0, cy - ph / 2.0, cx + pw / 2.0, cy + ph / 2.0);

        // Glass over the near-black viewfinder; a softly-raised surface over the
        // content background. Colours come from the theme in both cases.
        let (fill, title_col, sub_col, icon_col) = if glass {
            (
                p::alpha(c.neutral[1], 0.55),
                p::alpha(hud_ink(theme), 0.92),
                p::alpha(hud_ink(theme), 0.55),
                p::alpha(hud_ink(theme), 0.5),
            )
        } else {
            (
                p::alpha(c.surfaces.raised, 0.55),
                c.text_primary,
                c.text_dim,
                p::alpha(c.text_dim, 0.85),
            )
        };
        p::fill_round(scene, t, panel, 22.0, fill);
        p::stroke_round(
            scene,
            t,
            panel,
            22.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );

        let icx = panel.center().x;
        let icy = panel.y0 + 56.0;
        let icon_r = Rect::new(icx - 24.0, icy - 24.0, icx + 24.0, icy + 24.0);
        p::icon(scene, t, icon, icon_r, icon_col, 2.0);
        let ty = panel.y0 + 116.0;
        let title = tr.elide_to_width(title, 16.0, ui::num::f32_of_f64(panel.width() - 24.0));
        p::text_center(scene, tr, icx, ty, &title, 16.0, title_col);
        if let Some(sub) = subtitle {
            let sub = tr.elide_to_width(sub, 12.5, ui::num::f32_of_f64(panel.width() - 24.0));
            p::text_center(scene, tr, icx, ty + 24.0, &sub, 12.5, sub_col);
        }
        EmptyStateLayout {
            panel,
            icon: icon_r,
        }
    }

    /// A full-bleed "camera unavailable" placeholder for the camera screen
    /// when no live feed is present. Near-black so it reads as "camera off",
    /// honest rather than faking a feed; the message sits in a centred card so
    /// it stays anchored at wide widths.
    fn draw_no_feed(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        copy: (&str, &str),
    ) {
        let (title, subtitle) = copy;
        let c = &theme.tokens.colors;
        p::fill_rect(scene, t, rect, c.neutral[0]);
        // Centre the message in the whole window — the viewfinder is full-bleed.
        let layout = Self::draw_empty_state(
            scene,
            t,
            tr,
            theme,
            rect,
            &EmptyStateContent {
                icon: "camera",
                title,
                subtitle: Some(subtitle),
                glass: true,
                recovery: true,
            },
        );
        // Slash across the icon to read as "no camera".
        p::line(
            scene,
            t,
            layout.icon.x0 + 3.0,
            layout.icon.y0 + 3.0,
            layout.icon.x1 - 3.0,
            layout.icon.y1 - 3.0,
            p::alpha(hud_ink(theme), 0.5),
            2.2,
        );
        self.draw_camera_recovery_actions(scene, t, tr, theme, layout.panel);
    }

    fn draw_camera_recovery_actions(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        let devices = self.available_camera_devices();
        let mut actions = Vec::new();
        if self.camera_issue == Some(CameraIssue::PermissionDenied) {
            actions.push(("Open privacy settings", Action::OpenCameraPrivacySettings));
        }
        actions.push(("Refresh", Action::RetryCamera));
        if devices.len() > 1 {
            actions.push(("Choose camera", Action::OpenCameraChooser));
        }
        let count = actions.len();
        let gap = PILL_GAP;
        let max_w = panel.width() - 28.0;
        let width = ((max_w - gap * (count.saturating_sub(1) as f64)) / count as f64).max(44.0);
        let y = panel.y1 - 52.0;
        for (index, (label, action)) in actions.into_iter().enumerate() {
            let x = panel.x0 + 14.0 + index as f64 * (width + gap);
            let button = Rect::new(x, y, x + width, y + 38.0);
            // Keep the visual label inside a narrow two-action recovery row;
            // the a11y action retains the complete “Open camera privacy
            // settings” name from the shared Action mapping.
            let label = recovery_button_label(&action, label, width);
            let primary = matches!(
                action,
                Action::OpenCameraPrivacySettings | Action::RetryCamera
            );
            let face = if primary {
                theme.accent_primary
            } else {
                p::alpha(theme.tokens.colors.neutral[2], 0.92)
            };
            p::fill_round(scene, t, button, 12.0, face);
            p::stroke_round(scene, t, button, 12.0, surface::edge_on(face), 1.0);
            p::text_in(
                scene,
                tr,
                button,
                label,
                11.5,
                theme
                    .tokens
                    .colors
                    .readable_on(face, theme.tokens.colors.text_primary),
            );
            self.push_tap_region(button, action);
        }

        if self.camera_chooser_open && devices.len() > 1 {
            let row_h = 38.0;
            let chooser_h = devices.len() as f64 * row_h + 16.0;
            let chooser = Rect::new(
                panel.x0 + 14.0,
                panel.y1 + 10.0,
                panel.x1 - 14.0,
                panel.y1 + 10.0 + chooser_h,
            );
            // A floating chooser: Panel role ground + border (HIG §9.5).
            p::fill_round(scene, t, chooser, 14.0, ui::chrome::panel_ground(theme));
            p::stroke_round(
                scene,
                t,
                chooser,
                14.0,
                ui::chrome::panel_border(theme),
                1.0,
            );
            for (index, device) in devices.iter().enumerate() {
                let row = Rect::new(
                    chooser.x0 + 8.0,
                    chooser.y0 + 8.0 + index as f64 * row_h,
                    chooser.x1 - 8.0,
                    chooser.y0 + 8.0 + (index + 1) as f64 * row_h,
                );
                let selected = self.selected_camera.as_deref() == Some(device.as_str());
                if selected {
                    p::fill_round(scene, t, row, 10.0, theme.accent_primary);
                }
                let row_fill = if selected {
                    theme.accent_primary
                } else {
                    ui::chrome::panel_ground(theme)
                };
                p::text_in(
                    scene,
                    tr,
                    row,
                    device,
                    12.0,
                    theme
                        .tokens
                        .colors
                        .readable_on(row_fill, theme.tokens.colors.text_primary),
                );
                self.push_tap_region(row, Action::SelectCamera(device.clone()));
            }
        }
    }

    fn draw_hover_tooltip(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let action = self
            .regions
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(self.last_mouse))
            .map(|(_, action)| action);
        let Some(action) = action else {
            return;
        };
        // Visible text controls explain themselves. Tooltips are reserved for
        // the compact icon/image affordances whose meaning would otherwise be
        // pointer- and screen-reader-only.
        if !matches!(
            action,
            Action::Nav(Screen::Settings)
                | Action::Back
                | Action::Shutter
                | Action::CycleFacing
                | Action::ToggleGrid
        ) {
            return;
        }
        let node = self.a11y_node(Rect::ZERO, action);
        let Some(label) = node.name.as_deref() else {
            return;
        };
        let width = (f64::from(p::text_w(tr, label, 11.5)) + 22.0).min(260.0);
        let mut x = self.last_mouse.x + 14.0;
        let mut y = self.last_mouse.y + 18.0;
        if x + width > self.rect.x1 - 8.0 {
            x = self.rect.x1 - width - 8.0;
        }
        if y + 30.0 > self.rect.y1 - 8.0 {
            y = self.last_mouse.y - 38.0;
        }
        let rect = Rect::new(
            x.max(self.rect.x0 + 8.0),
            y.max(self.rect.y0 + 8.0),
            x + width,
            y + 30.0,
        );
        p::fill_round(
            scene,
            t,
            rect,
            9.0,
            p::alpha(theme.tokens.colors.neutral[0], 0.94),
        );
        p::stroke_round(
            scene,
            t,
            rect,
            9.0,
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 30),
            1.0,
        );
        p::text_in(scene, tr, rect, label, 11.5, hud_ink(theme));
    }

    fn draw_toast(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let Some(msg) = self.toast.as_deref() else {
            return;
        };
        // Do not colour device/file failures as successes. The app keeps a
        // compact text toast state, so classify only the truthful failure
        // prefixes produced by its own operation paths.
        let kind = if msg.starts_with("Save failed")
            || msg.starts_with("Couldn't")
            || msg.starts_with("Could not")
            || msg.starts_with("Text recognition")
            || msg.starts_with("Text could not")
        {
            ToastKind::Error
        } else if msg.starts_with("Connect a camera")
            || msg.starts_with("Nothing recorded")
            || msg.starts_with("No code found")
        {
            ToastKind::Warning
        } else {
            ToastKind::Success
        };
        let reserved_bottom = match self.screen {
            Screen::Document => 130.0,
            Screen::Camera | Screen::Scanner => 64.0,
            Screen::Settings => 0.0,
        };
        let area = if reserved_bottom > 0.0 {
            Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x1,
                self.rect.y1 - reserved_bottom,
            )
        } else {
            self.rect
        };
        paint_transient(
            scene,
            t,
            tr,
            theme,
            area,
            ToastAnchor::BottomCenter,
            &ToastCard::new(msg, kind),
        );
    }

    /// A slim, non-interactive scroll indicator pinned to the right edge of
    /// `area`. Only drawn when the content overflows.
    fn draw_scrollbar(
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        area: Rect,
        metrics: ScrollMetrics,
    ) {
        let ScrollMetrics {
            scroll,
            max_scroll,
            view_h,
            content_h,
        } = metrics;
        if max_scroll <= 0.0 || content_h <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let track = Rect::new(area.x1 - 6.0, area.y0 + 6.0, area.x1 - 3.0, area.y1 - 6.0);
        let vh = track.height();
        let thumb_h = (vh * (view_h / content_h)).clamp(28.0, vh);
        let frac = (scroll / max_scroll).clamp(0.0, 1.0);
        let ty = track.y0 + (vh - thumb_h) * frac;
        p::fill_round(
            scene,
            t,
            Rect::new(track.x0, ty, track.x1, ty + thumb_h),
            1.5,
            p::alpha(c.text_dim, 0.55),
        );
    }

    fn draw_camera(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let has_feed = self.feed_active();
        let recording = self.is_recording();

        // 1. Edge-to-edge viewfinder — the live preview bleeds to every window
        // edge; every control floats over it rather than living in a panel.
        if has_feed {
            let vw = full.width().max(2.0) as u32;
            let vh = full.height().max(2.0) as u32;
            match &self.storage {
                Storage::Sample => {
                    let seed = Self::viewfinder_seed(self.facing);
                    let need = self
                        .cam_view
                        .as_ref()
                        .is_none_or(|(w, h, s, _)| (*w, *h, *s) != (vw, vh, seed));
                    if need {
                        self.cam_view = Some((
                            vw,
                            vh,
                            seed,
                            frame_image_fill(frame::viewfinder(seed, vw, vh)),
                        ));
                    }
                    if let Some((_, _, _, img)) = self.cam_view.as_mut() {
                        img.paint_at(scene, t, full);
                    }
                }
                Storage::Disk(_) => {
                    let cur_gen = self.frame_gen;
                    if self.live_view.as_ref().is_none_or(|(g, _)| *g != cur_gen)
                        && let Some(fb) = self.latest_frame.clone()
                    {
                        self.live_view = Some((cur_gen, frame_image_cover(fb)));
                    }
                    if let Some((_, img)) = self.live_view.as_mut() {
                        img.paint_at(scene, t, full);
                    }
                }
            }
            self.draw_viewfinder_overlays(scene, t, theme, full);
            // Legibility washes: a soft dark fade top and bottom so white
            // controls stay readable over any scene — no opaque panels.
            let blk = media_scrim(theme);
            p::vscrim(
                scene,
                t,
                Rect::new(full.x0, full.y0, full.x1, full.y0 + 104.0),
                blk,
                0.42,
                0.0,
            );
            p::vscrim(
                scene,
                t,
                Rect::new(full.x0, full.y1 - 200.0, full.x1, full.y1),
                blk,
                0.0,
                0.6,
            );
            self.draw_camera_top_controls(scene, t, tr, theme, full);
            if recording {
                self.draw_rec_indicator(scene, t, tr, full, theme);
            } else if self.is_counting_down() {
                self.draw_timer_indicator(scene, t, tr, full, theme);
            }
        } else {
            let (title, subtitle) = self.no_feed_copy();
            self.draw_no_feed(scene, t, tr, theme, full, (title, subtitle));
        }

        // Output is always owned by Photos.  This is a typed cross-app launch,
        // not a Camera gallery: the thumbnail below opens the latest capture,
        // while this compact surface opens the library at its true owner.
        self.glass_button(
            scene,
            t,
            theme,
            Circle::new(Point::new(full.x1 - EDGE - 58.0, full.y0 + 35.0), 16.0),
            "image",
            Action::OpenPhotos,
        );
        // Settings gear, top-right — opens Settings as an overlay sheet. Drawn
        // whether or not a feed is present so Settings is always reachable.
        // Centred on the top pill row: both sit on the `EDGE` corner inset's
        // 32 px circle, so they share one centre line at every text scale.
        let row_cy = full.y0 + EDGE + 16.0;
        self.glass_button(
            scene,
            t,
            theme,
            Circle::new(Point::new(full.x1 - EDGE - 16.0, row_cy), 16.0),
            "settings",
            Action::Nav(Screen::Settings),
        );

        // 2. Bottom capture cluster (always drawn; disabled without a feed).
        self.draw_capture_cluster(
            scene,
            t,
            tr,
            theme,
            full,
            CaptureClusterState {
                has_feed,
                recording,
            },
        );
    }

    fn draw_viewfinder_overlays(&self, scene: &mut Scene, t: Affine, theme: &Theme, vf: Rect) {
        // Rule-of-thirds grid.
        if self.settings.grid {
            let gc = p::alpha(hud_ink(theme), 0.22);
            for k in 1..3 {
                let x = vf.x0 + vf.width() * f64::from(k) / 3.0;
                p::line(scene, t, x, vf.y0, x, vf.y1, gc, 1.0);
                let y = vf.y0 + vf.height() * f64::from(k) / 3.0;
                p::line(scene, t, vf.x0, y, vf.x1, y, gc, 1.0);
            }
        }
        // Level horizon indicator (only when enabled — no permanent reticle
        // cluttering the centre of the frame).
        if self.settings.level {
            let y = vf.center().y;
            p::line(
                scene,
                t,
                vf.center().x - 60.0,
                y,
                vf.center().x + 60.0,
                y,
                p::alpha(theme.status_success, 0.9),
                2.0,
            );
            p::line(
                scene,
                t,
                vf.center().x - 92.0,
                y,
                vf.center().x - 70.0,
                y,
                p::alpha(hud_ink(theme), 0.55),
                2.0,
            );
            p::line(
                scene,
                t,
                vf.center().x + 70.0,
                y,
                vf.center().x + 92.0,
                y,
                p::alpha(hud_ink(theme), 0.55),
                2.0,
            );
        }
    }

    /// Height of every pill in the top control row: the authored
    /// [`CTRL_PILL_MIN_H`] at 1×, grown to contain the measured 12 px label
    /// line plus [`CTRL_PILL_PAD_Y`] at an accessibility text scale. The
    /// settings button centres on the same row.
    fn ctrl_pill_height(tr: &mut TextRenderer) -> f64 {
        (p::text_h(tr, "HDR", CTRL_PILL_LABEL_SIZE) + CTRL_PILL_PAD_Y * 2.0).max(CTRL_PILL_MIN_H)
    }

    /// A floating glass control pill (leading icon optional). `active` fills it
    /// with the accent; otherwise it's dark glass with a hairline border.
    /// Returns the pill's rect so the caller can register a hit region.
    fn ctrl_pill(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pos: Point,
        content: PillContent,
    ) -> Rect {
        let PillContent {
            icon,
            label,
            active,
        } = content;
        let colors = &theme.tokens.colors;
        let large_type = theme.text_scale >= 1.5;
        let pad = if large_type { 14.0 } else { 11.0 };
        let icon_w = if icon.is_some() { 17.0 } else { 0.0 };
        let tw = if label.is_empty() {
            0.0
        } else {
            f64::from(p::text_w(tr, label, CTRL_PILL_LABEL_SIZE))
        };
        let gap = if icon.is_some() && !label.is_empty() {
            5.0
        } else {
            0.0
        };
        let width = pad * 2.0 + icon_w + gap + tw;
        // One height for the whole row — labelled and icon-only pills alike —
        // so the row reads as one control strip at every text scale.
        let height = Self::ctrl_pill_height(tr);
        let th = p::text_h(tr, "HDR", CTRL_PILL_LABEL_SIZE);
        let rect = Rect::new(pos.x, pos.y, pos.x + width, pos.y + height);
        if active {
            p::fill_round(scene, t, rect, height / 2.0, theme.accent_primary);
        } else {
            p::fill_round(
                scene,
                t,
                rect,
                height / 2.0,
                p::alpha(colors.neutral[0], 0.5),
            );
            p::stroke_round(
                scene,
                t,
                rect,
                height / 2.0,
                ui::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
                1.0,
            );
        }
        let fg = if active {
            hud_ink(theme)
        } else {
            p::alpha(hud_ink(theme), 0.92)
        };
        let mut cx = pos.x + pad;
        if let Some(ic) = icon {
            p::icon(
                scene,
                t,
                ic,
                Rect::new(cx, rect.center().y - 8.5, cx + 17.0, rect.center().y + 8.5),
                fg,
                1.7,
            );
            cx += icon_w + gap;
        }
        if !label.is_empty() {
            // Centre on the measured line, not on `size - k`.
            p::text(
                scene,
                tr,
                cx,
                rect.center().y - th / 2.0,
                label,
                CTRL_PILL_LABEL_SIZE,
                fg,
            );
        }
        rect
    }

    /// The quick-control row floating along the top of the viewfinder:
    /// resolution, HDR, self-timer and grid — each tappable, so the settings
    /// most-used while shooting are one tap away rather than buried.
    fn draw_camera_top_controls(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let mut x = full.x0 + EDGE;
        // The pill row shares the settings gear's centre line (a 32 px circle
        // on the `EDGE` corner inset), so the four corners read as one frame.
        let y = full.y0 + EDGE + 16.0 - Self::ctrl_pill_height(tr) / 2.0;
        let gap = PILL_GAP;

        let r = Self::ctrl_pill(
            scene,
            t,
            tr,
            theme,
            Point::new(x, y),
            PillContent {
                icon: None,
                label: self.settings.resolution.label(),
                active: false,
            },
        );
        self.push_tap_region(r, Action::CycleResolution);
        x = r.x1 + gap;

        // At extreme narrow/high-zoom widths, keep the capture surface clear
        // rather than clipping a row of tiny pills.  Every hidden quick
        // control remains in the Settings destination from the shared drawer.
        let compact_controls = full.width() < 420.0 || theme.text_scale >= 1.5;
        if !compact_controls && full.width() >= 300.0 {
            let r = Self::ctrl_pill(
                scene,
                t,
                tr,
                theme,
                Point::new(x, y),
                PillContent {
                    icon: None,
                    label: "HDR",
                    active: self.settings.hdr,
                },
            );
            self.push_tap_region(r, Action::ToggleHdr);
            x = r.x1 + gap;
        }

        let next_timer = cycle(&Timer::ALL, self.settings.timer);
        let tlabel = if self.settings.timer == Timer::Off {
            "Timer".to_string()
        } else {
            self.settings.timer.label().to_string()
        };
        let r = Self::ctrl_pill(
            scene,
            t,
            tr,
            theme,
            Point::new(x, y),
            PillContent {
                icon: Some("timer"),
                label: &tlabel,
                active: self.settings.timer != Timer::Off,
            },
        );
        self.push_tap_region(r, Action::SetTimer(next_timer));
        x = r.x1 + gap;

        if !compact_controls && full.width() >= 340.0 {
            let r = Self::ctrl_pill(
                scene,
                t,
                tr,
                theme,
                Point::new(x, y),
                PillContent {
                    icon: Some("grid"),
                    label: "",
                    active: self.settings.grid,
                },
            );
            self.push_tap_region(r, Action::ToggleGrid);
        }
    }

    /// The bottom capture cluster floating over the viewfinder: a recent handoff
    /// thumbnail peek, the shutter, a lens switch, and — when idle — the
    /// Photo/Video/Scan mode switcher above the shutter.
    fn draw_capture_cluster(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        state: CaptureClusterState,
    ) {
        let scx = self.stage_center_x();
        let cy = full.y1 - 64.0;

        // Photo / Video / Scan switcher above the shutter — hidden while
        // recording (mode is locked; the stop-square shutter + REC badge carry
        // the state).
        if !state.recording {
            // The switcher is the shutter's sibling: its bottom edge keeps
            // `s3` above the shutter's top (`cy - 34`). Its segments are
            // 44 px tap targets centred on the 34 px capsule, so anything
            // closer put their hit boxes 5 px into the shutter's (HIG §7
            // "nothing touches" — the geometry audit flagged the overlap).
            self.draw_mode_switcher(scene, t, tr, theme, scx, cy - 34.0 - ACTION_GAP);
        }
        self.draw_recent_capture_peek(scene, t, tr, theme, full, cy);
        self.draw_shutter(scene, t, theme, scx, cy, state);
        self.draw_lens_switch(scene, t, tr, theme, full, cy);
    }

    /// One contextual capture handoff, bottom-left of the capture cluster.
    ///
    /// This is intentionally not a gallery tile: Camera keeps no roll or
    /// count. At desktop widths the owner is named directly; compact layouts
    /// retain the familiar small preview and expose the full destination to
    /// assistive technology.
    fn draw_recent_capture_peek(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        cy: f64,
    ) {
        let ts = 30.0;
        // Same edge inset as every other floating corner control (the top
        // pills and the settings button), so the four corners read as one
        // frame around the viewfinder.
        let thumb = Rect::new(full.x0 + EDGE, cy - ts, full.x0 + EDGE + ts * 2.0, cy + ts);
        if let Some(cap) = self.captures.first().cloned() {
            self.push_region(thumb, Action::OpenRecentCapture);
            self.ensure_thumb(&cap);
            p::push_clip_round(scene, t, thumb, 14.0);
            if let Some(img) = self.thumbs.get_mut(&cap.id) {
                img.paint_at(scene, t, thumb);
            }
            p::pop(scene);
            p::stroke_round(scene, t, thumb, 14.0, p::alpha(hud_ink(theme), 0.65), 1.5);
            // The named handoff makes Camera's ownership boundary obvious at
            // a glance without squeezing the shutter at phone widths.
            if full.width() >= 520.0 && theme.text_scale < 1.5 {
                let (_, destination) = crate::actions::capture_destination(cap.is_video());
                let label = format!("Open in {destination}");
                let label_w = (f64::from(p::text_w(tr, &label, 11.5)) + 28.0).min(154.0);
                let label_x = thumb.x1 + 8.0;
                let label_rect = Rect::new(label_x, cy - 16.0, label_x + label_w, cy + 16.0);
                // Do not let a destination label graze the shutter at the
                // transitional tablet width. It remains a named screen-reader
                // control even when visual density asks us to keep the preview
                // compact.
                if label_rect.x1 <= self.stage_center_x() - 42.0 {
                    p::fill_round(scene, t, label_rect, 16.0, p::alpha(hud_ink(theme), 0.2));
                    p::text_in(scene, tr, label_rect, &label, 11.5, hud_ink(theme));
                    self.push_tap_region(label_rect, Action::OpenRecentCapture);
                }
            }
        }
    }

    /// Shutter (centre of the capture cluster). Photo: white disc. Video: red
    /// dot → red stop-square while recording.
    fn draw_shutter(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        scx: f64,
        cy: f64,
        state: CaptureClusterState,
    ) {
        let c = &theme.tokens.colors;
        let danger = c.semantic.danger;
        let outer = 34.0;
        let ring = if state.recording {
            danger
        } else if state.has_feed {
            p::alpha(hud_ink(theme), 0.95)
        } else {
            p::alpha(hud_ink(theme), 0.4)
        };
        p::circle_stroke(scene, t, scx, cy, outer, ring, 4.5);
        match self.mode {
            CaptureMode::Photo => {
                let core = if state.has_feed {
                    hud_ink(theme)
                } else {
                    p::alpha(hud_ink(theme), 0.4)
                };
                p::circle(scene, t, scx, cy, outer - 8.0, core);
            }
            CaptureMode::Video if state.recording => {
                let s = 14.0;
                p::fill_round(
                    scene,
                    t,
                    Rect::new(scx - s, cy - s, scx + s, cy + s),
                    5.0,
                    danger,
                );
            }
            CaptureMode::Video => {
                let core = if state.has_feed {
                    danger
                } else {
                    p::alpha(danger, 0.4)
                };
                p::circle(scene, t, scx, cy, outer - 10.0, core);
            }
        }
        self.push_region(
            Rect::new(scx - outer, cy - outer, scx + outer, cy + outer),
            Action::Shutter,
        );
    }

    /// Lens switch (bottom-right of the capture cluster) — cycles rear /
    /// front / ultra-wide.
    fn draw_lens_switch(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        cy: f64,
    ) {
        let c = &theme.tokens.colors;
        // The 26 px circle's outer edge sits on the same inset as the
        // top-right settings button, so both right-hand corners align.
        let lx = full.x1 - EDGE - 26.0;
        p::circle(scene, t, lx, cy, 26.0, p::alpha(c.neutral[0], 0.5));
        p::circle_stroke(
            scene,
            t,
            lx,
            cy,
            26.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        p::icon(
            scene,
            t,
            "switch",
            Rect::new(lx - 13.0, cy - 13.0, lx + 13.0, cy + 13.0),
            hud_ink(theme),
            2.0,
        );
        p::text_center(
            scene,
            tr,
            lx,
            cy + 34.0,
            self.facing.label(),
            10.0,
            p::alpha(hud_ink(theme), 0.78),
        );
        self.push_region(
            Rect::new(lx - 26.0, cy - 26.0, lx + 26.0, cy + 26.0),
            Action::CycleFacing,
        );
    }

    /// A red "● REC m:ss" badge floating at the top-centre of the stage while
    /// a clip is recording.
    fn draw_rec_indicator(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        full: Rect,
        theme: &Theme,
    ) {
        let scx = self.stage_center_x();
        let started = self.recording.as_ref().map_or(0, |s| s.started);
        let elapsed = (now_secs() - started).max(0);
        let label = format!("REC  {}", fmt_dur(elapsed * 1000));
        let red = theme.tokens.colors.semantic.danger;
        let tw = f64::from(p::text_w(tr, &label, 12.5));
        let w = 20.0 + 12.0 + tw + 14.0;
        let pill = Rect::new(
            scx - w / 2.0,
            full.y0 + EDGE,
            scx + w / 2.0,
            full.y0 + EDGE + 28.0,
        );
        p::fill_round(scene, t, pill, 14.0, p::alpha(media_scrim(theme), 0.55));
        p::stroke_round(scene, t, pill, 14.0, p::alpha(red, 0.55), 1.0);
        p::circle(scene, t, pill.x0 + 17.0, pill.center().y, 5.0, red);
        p::text(
            scene,
            tr,
            pill.x0 + 29.0,
            pill.center().y - 8.0,
            &label,
            12.5,
            hud_ink(theme),
        );
    }

    /// High-contrast, calm timer feedback.  It uses the same discrete cadence
    /// as the operating countdown (and settles to that cadence under reduced
    /// motion), instead of introducing a decorative looping animation.
    fn draw_timer_indicator(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        full: Rect,
        theme: &Theme,
    ) {
        let Some(countdown) = self.countdown.as_ref() else {
            return;
        };
        let duration = countdown.due.saturating_duration_since(ui::time::now());
        let remaining = (duration.as_secs() + u64::from(duration.subsec_nanos() != 0)).max(1);
        let label = format!("{remaining}  ·  Tap shutter to cancel");
        let tw = f64::from(p::text_w(tr, &label, 12.5));
        let w = tw + 38.0;
        let pill = Rect::new(
            self.stage_center_x() - w / 2.0,
            full.y0 + 18.0,
            self.stage_center_x() + w / 2.0,
            full.y0 + 50.0,
        );
        p::fill_round(
            scene,
            t,
            pill,
            16.0,
            p::alpha(theme.tokens.colors.neutral[0], 0.72),
        );
        p::stroke_round(
            scene,
            t,
            pill,
            16.0,
            p::alpha(theme.accent_primary, 0.7),
            1.0,
        );
        p::circle(
            scene,
            t,
            pill.x0 + 17.0,
            pill.center().y,
            5.0,
            theme.accent_primary,
        );
        p::text(
            scene,
            tr,
            pill.x0 + 29.0,
            pill.center().y - 8.0,
            &label,
            12.5,
            hud_ink(theme),
        );
    }

    fn ensure_thumb(&mut self, cap: &Capture) {
        if self.thumbs.contains_key(&cap.id) && !self.thumb_pending.contains_key(&cap.id) {
            return;
        }
        if self.thumb_pending.contains_key(&cap.id) || self.thumb_failed.contains(&cap.id) {
            return;
        }
        if matches!(self.storage, Storage::Sample) {
            if self.thumbs.len() >= MAX_THUMB_CACHE {
                self.thumbs.clear();
            }
            let (w, h) = THUMB;
            self.thumbs.insert(
                cap.id.clone(),
                frame_image_cover(frame::thumbnail(cap.scene, w, h)),
            );
            return;
        }
        let (w, h) = THUMB;
        let size = thumbnail::ThumbSize::new(w, h);
        // `media_id` names verified bytes in the content store, and is the
        // preview's content address; a local path is admitted only for the
        // recent capture projection.
        let media = cap
            .media_id
            .as_ref()
            .filter(|media_id| crate::media::is_media_id(media_id))
            .cloned();
        let address_key = media.as_deref().and_then(|id| {
            thumbnail::ThumbKey::of_address(id.strip_prefix(crate::media::MEDIA_PREFIX)?, size)
        });
        let request = match (&cap.path, media) {
            (Some(path), _) => {
                let key = address_key.or_else(|| {
                    let meta = crate::files::metadata(path)?;
                    let mtime_ms = meta
                        .modified
                        .duration_since(ui::time::UNIX_EPOCH)
                        .ok()?
                        .as_millis();
                    Some(thumbnail::ThumbKey::of_path_meta(
                        path,
                        u64::try_from(mtime_ms).unwrap_or(u64::MAX),
                        meta.len,
                        size,
                    ))
                });
                key.map(|key| {
                    thumbnail::ThumbRequest::file(
                        key,
                        thumbnail::category_for_path(path),
                        path.clone(),
                    )
                })
            }
            (None, Some(id)) => address_key.map(|key| {
                let content = self.content.clone();
                thumbnail::ThumbRequest::load(key, thumbnail::FileCategory::Image, move || {
                    crate::media::read(&content, &id)
                })
            }),
            (None, None) => None,
        };
        let Some(request) = request else {
            self.thumb_failed.insert(cap.id.clone());
            return;
        };
        let key = request.key();
        if let thumbnail::ThumbState::Badge(_) = self.thumb_service.request(request) {
            self.thumb_failed.insert(cap.id.clone());
            return;
        }
        self.thumb_pending.insert(cap.id.clone(), key);
        // A low-cost deterministic placeholder means the card never flashes
        // blank while the shared pipeline is working.
        if self.thumbs.len() >= MAX_THUMB_CACHE {
            self.thumbs.clear();
        }
        self.thumbs.insert(
            cap.id.clone(),
            frame_image_cover(frame::thumbnail(cap.scene, w, h)),
        );
    }
}

// ====================================================================
// Document scanner and code scanner screens
// ====================================================================

impl CameraApp {
    #[allow(
        clippy::too_many_lines,
        reason = "the document workspace is one coordinated custom-painted semantic layout"
    )]
    fn draw_document_scanner(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let c = &theme.tokens.colors;
        p::fill_rect(scene, t, full, c.surfaces.base);
        let compact = full.width() < WIDE_MIN;
        let content = shrink(full, EDGE);
        p::text(
            scene,
            tr,
            content.x0,
            content.y0 + 4.0,
            "Scan documents",
            if compact { 25.0 } else { 30.0 },
            c.text_primary,
        );
        p::text(
            scene,
            tr,
            content.x0,
            content.y0 + 40.0,
            if compact {
                "Scan, reorder, and edit pages."
            } else {
                "Capture a multi-page document and keep every page editable."
            },
            13.0,
            c.text_dim,
        );

        let workspace = Rect::new(content.x0, content.y0 + 78.0, content.x1, content.y1 - 70.0);
        let controls_w = if compact { workspace.width() } else { 286.0 };
        let controls = Rect::new(
            workspace.x0,
            workspace.y0,
            workspace.x0 + controls_w,
            if compact {
                workspace.y0 + 268.0
            } else {
                workspace.y1
            },
        );
        p::fill_round(scene, t, controls, 16.0, c.surfaces.raised);
        let scanner_name = self
            .selected_scanner
            .as_ref()
            .and_then(|selected| {
                self.scanner_devices
                    .iter()
                    .find(|scanner| scanner.device == *selected)
            })
            .map_or("No scanner found", |scanner| scanner.description.as_str());
        let scanner_rect = Rect::new(
            controls.x0 + 18.0,
            controls.y0 + 14.0,
            controls.x1 - 18.0,
            controls.y0 + 56.0,
        );
        if let Some(anchor) = self
            .document_choice_anchors
            .get_mut(DocumentChoice::Scanner.index())
        {
            *anchor = scanner_rect;
        }
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(scanner_rect, scanner_name)
                .icon(ui::icons::Icon::ChevronDown)
                .hovered(scanner_rect.contains(self.last_mouse))
                .disabled(self.scanner_devices.is_empty() || self.document_job.is_some()),
        );
        if !self.scanner_devices.is_empty() && self.document_job.is_none() {
            self.push_region(
                scanner_rect,
                Action::OpenDocumentChoice(DocumentChoice::Scanner),
            );
        }
        let gap = 10.0;
        let setting_w = (controls.width() - 46.0) / 2.0;
        let left = controls.x0 + 18.0;
        let right = left + setting_w + gap;
        let settings = [
            (
                "RESOLUTION",
                format!("{} DPI", self.document_settings.scan.resolution_dpi),
                Rect::new(
                    left,
                    controls.y0 + 82.0,
                    left + setting_w,
                    controls.y0 + 126.0,
                ),
                Action::OpenDocumentChoice(DocumentChoice::Resolution),
            ),
            (
                "COLOR",
                scan_color_label(self.document_settings.scan.color).to_owned(),
                Rect::new(
                    right,
                    controls.y0 + 82.0,
                    right + setting_w,
                    controls.y0 + 126.0,
                ),
                Action::OpenDocumentChoice(DocumentChoice::Color),
            ),
            (
                "PAPER SOURCE",
                scan_source_label(self.document_settings.scan.source).to_owned(),
                Rect::new(
                    left,
                    controls.y0 + 148.0,
                    left + setting_w,
                    controls.y0 + 192.0,
                ),
                Action::OpenDocumentChoice(DocumentChoice::Source),
            ),
            (
                "TEXT LANGUAGE",
                self.document_settings.ocr_language.label().to_owned(),
                Rect::new(
                    right,
                    controls.y0 + 148.0,
                    right + setting_w,
                    controls.y0 + 192.0,
                ),
                Action::OpenDocumentChoice(DocumentChoice::Language),
            ),
        ];
        for (label, value, rect, action) in settings {
            if let Action::OpenDocumentChoice(choice) = &action
                && let Some(anchor) = self.document_choice_anchors.get_mut(choice.index())
            {
                *anchor = rect;
            }
            p::text(scene, tr, rect.x0, rect.y0 - 17.0, label, 10.0, c.text_dim);
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, &value)
                    .icon(ui::icons::Icon::ChevronDown)
                    .hovered(rect.contains(self.last_mouse))
                    .disabled(self.document_job.is_some()),
            );
            if self.document_job.is_none() {
                self.push_region(rect, action);
            }
        }
        let scan_button = Rect::new(
            controls.x0 + 18.0,
            controls.y0 + 206.0,
            controls.x1 - 18.0,
            controls.y0 + 250.0,
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(
                scan_button,
                if self.document_job_kind == Some(DocumentJobKind::Capture) {
                    "Scanning…"
                } else {
                    "Scan page"
                },
            )
            .icon(ui::icons::Icon::FileText)
            .primary()
            .hovered(scan_button.contains(self.last_mouse))
            .disabled(self.selected_scanner.is_none() || self.document_job.is_some()),
        );
        if self.selected_scanner.is_some() && self.document_job.is_none() {
            self.push_region(scan_button, Action::CaptureDocument);
        }

        let preview = if compact {
            Rect::new(workspace.x0, controls.y1 + 14.0, workspace.x1, workspace.y1)
        } else {
            Rect::new(controls.x1 + 16.0, workspace.y0, workspace.x1, workspace.y1)
        };
        p::fill_round(scene, t, preview, 16.0, c.surfaces.raised);
        if self.document_scan.pages.is_empty() {
            let icon = Rect::new(
                preview.center().x - 22.0,
                preview.center().y - 58.0,
                preview.center().x + 22.0,
                preview.center().y - 14.0,
            );
            ui::icons::Icon::FileText.draw(scene, t, icon, c.text_dim);
            p::text_center(
                scene,
                tr,
                preview.center().x,
                preview.center().y + 4.0,
                "Your scanned pages appear here",
                15.0,
                c.text_primary,
            );
            p::text_center(
                scene,
                tr,
                preview.center().x,
                preview.center().y + 30.0,
                "Start with one page; add and reorder more at any time.",
                12.0,
                c.text_dim,
            );
        } else {
            let document_busy = self.document_job.is_some();
            let can_move_earlier = !document_busy && self.document_scan.selected > 0;
            let can_move_later =
                !document_busy && self.document_scan.selected + 1 < self.document_scan.pages.len();
            for (rect, icon, action, enabled) in [
                (
                    Rect::new(
                        preview.x1 - 108.0,
                        preview.y0 + 8.0,
                        preview.x1 - 60.0,
                        preview.y0 + 52.0,
                    ),
                    ui::icons::Icon::ChevronLeft,
                    Action::MoveDocumentPageEarlier,
                    can_move_earlier,
                ),
                (
                    Rect::new(
                        preview.x1 - 56.0,
                        preview.y0 + 8.0,
                        preview.x1 - 8.0,
                        preview.y0 + 52.0,
                    ),
                    ui::icons::Icon::ChevronRight,
                    Action::MoveDocumentPageLater,
                    can_move_later,
                ),
            ] {
                Button::paint(
                    scene,
                    t,
                    tr,
                    theme,
                    &ButtonFace::new(rect, "")
                        .icon(icon)
                        .hovered(rect.contains(self.last_mouse))
                        .disabled(!enabled),
                );
                if enabled {
                    self.push_region(rect, action);
                }
            }
            if self.document_crop_editor.is_none() && !document_busy {
                let crop_button = Rect::new(
                    preview.x1 - 196.0,
                    preview.y0 + 8.0,
                    preview.x1 - 116.0,
                    preview.y0 + 52.0,
                );
                Button::paint(
                    scene,
                    t,
                    tr,
                    theme,
                    &ButtonFace::new(crop_button, "Crop")
                        .icon(ui::icons::Icon::Scissors)
                        .hovered(crop_button.contains(self.last_mouse)),
                );
                self.push_region(crop_button, Action::BeginDocumentCrop);
            }
            let paper_width = (preview.width() - 64.0).min(330.0);
            let paper_height = (preview.height() - 142.0).min(paper_width * 1.414);
            let paper = Rect::new(
                preview.center().x - paper_width / 2.0,
                preview.center().y - paper_height / 2.0 - 24.0,
                preview.center().x + paper_width / 2.0,
                preview.center().y + paper_height / 2.0 - 24.0,
            );
            if let Some(editor) = self.document_crop_editor.as_mut() {
                editor.paint_at(scene, t, theme, paper);
            } else {
                p::fill_round(scene, t, paper, 5.0, Color::from_rgb8(246, 246, 242));
                let selected = self.document_scan.selected;
                let page_key = self
                    .document_scan
                    .pages
                    .get(selected)
                    .map(|page| (page.crop, page.rotation));
                let preview_is_current = self.document_preview.as_ref().is_some_and(
                    |(cached_index, cached_crop, cached_rotation, _)| {
                        Some((*cached_crop, *cached_rotation)) == page_key
                            && *cached_index == selected
                    },
                );
                if !preview_is_current && let Some(page) = self.document_scan.pages.get(selected) {
                    let rendered = page.rendered().to_rgba8();
                    let (width, height) = rendered.dimensions();
                    self.document_preview = Some((
                        selected,
                        page.crop,
                        page.rotation,
                        Image::from_rgba(Arc::from(rendered.into_raw()), width, height)
                            .with_fit(ImageFit::Contain),
                    ));
                }
                if let Some((_, _, _, image)) = self.document_preview.as_mut() {
                    image.paint_at(scene, t, paper);
                }
            }
            p::stroke_round(scene, t, paper, 5.0, c.border_standard, 1.0);
            p::text(
                scene,
                tr,
                preview.x0 + 16.0,
                preview.y0 + 14.0,
                &format!(
                    "Page {} of {}",
                    self.document_scan.selected + 1,
                    self.document_scan.pages.len()
                ),
                12.0,
                c.text_dim,
            );
            let text_status = self.document_text_status();
            let text_is_stale = self
                .document_scan
                .pages
                .get(self.document_scan.selected)
                .and_then(|page| page.ocr.as_ref())
                .is_some_and(|document| {
                    document.language != self.document_settings.ocr_language.code()
                });
            p::text(
                scene,
                tr,
                preview.x0 + 16.0,
                preview.y0 + 34.0,
                &text_status,
                11.0,
                if text_is_stale {
                    c.semantic.warning
                } else {
                    c.text_dim
                },
            );
            let actions_width = (preview.width() - 32.0).min(360.0);
            let actions = Rect::new(
                preview.center().x - actions_width / 2.0,
                preview.y1 - 60.0,
                preview.center().x + actions_width / 2.0,
                preview.y1 - 16.0,
            );
            let crop_active = self.document_crop_editor.is_some();
            let action_specs: Vec<(&str, ui::icons::Icon, Action, bool)> = if crop_active {
                vec![
                    (
                        "Cancel",
                        ui::icons::Icon::X,
                        Action::CancelDocumentCrop,
                        false,
                    ),
                    (
                        "Apply",
                        ui::icons::Icon::Check,
                        Action::ApplyDocumentCrop,
                        false,
                    ),
                ]
            } else {
                let has_text = self
                    .document_scan
                    .pages
                    .get(self.document_scan.selected)
                    .and_then(|page| page.ocr.as_ref())
                    .is_some();
                let mut specs = vec![
                    (
                        "Turn",
                        ui::icons::Icon::RotateCw,
                        Action::RotateDocument,
                        false,
                    ),
                    (
                        if self.document_job_kind == Some(DocumentJobKind::Recognize) {
                            "Reading…"
                        } else {
                            "Read"
                        },
                        ui::icons::Icon::FileText,
                        Action::RecognizeDocument,
                        false,
                    ),
                ];
                if has_text {
                    specs.push(("", ui::icons::Icon::Copy, Action::CopyDocumentText, false));
                }
                specs.extend([
                    (
                        "Save",
                        ui::icons::Icon::Download,
                        Action::ExportDocument,
                        false,
                    ),
                    (
                        "",
                        ui::icons::Icon::Trash2,
                        Action::RemoveDocumentPage,
                        true,
                    ),
                ]);
                specs
            };
            let gap = 8.0;
            let slot_width = (actions.width()
                - gap * (action_specs.len().saturating_sub(1)) as f64)
                / action_specs.len() as f64;
            for (index, (label, icon, action, danger)) in action_specs.into_iter().enumerate() {
                let rect = Rect::new(
                    actions.x0 + index as f64 * (slot_width + gap),
                    actions.y0,
                    actions.x0 + index as f64 * (slot_width + gap) + slot_width,
                    actions.y1,
                );
                let mut face = ButtonFace::new(rect, label)
                    .icon(icon)
                    .hovered(rect.contains(self.last_mouse))
                    .disabled(document_busy);
                if danger {
                    face = face.danger();
                } else if matches!(&action, Action::ApplyDocumentCrop) {
                    face = face.primary();
                }
                Button::paint(scene, t, tr, theme, &face);
                if !document_busy {
                    self.push_region(rect, action);
                }
            }
        }

        self.draw_mode_switcher(scene, t, tr, theme, full.center().x, full.y1 - 56.0);
    }

    fn draw_scanner(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let live = self.feed_active();
        let wide = full.width() >= WIDE_MIN;
        let panel_w = if wide { 300.0 } else { 0.0 };
        let panel_gap = if wide { 14.0 } else { 0.0 };
        // The scan stage: the full width minus the optional recent-scans panel.
        // The scan window + result card centre here.
        let stage = Rect::new(
            full.x0,
            full.y0,
            full.x1 - EDGE - panel_w - panel_gap,
            full.y1,
        );

        if live {
            self.draw_scanner_feed(scene, t, tr, theme, full, stage);
        } else {
            self.draw_scanner_offline(scene, t, tr, theme, full, stage);
        }

        // Floating title, top-left over the feed.
        let title = if theme.text_scale >= 1.5 {
            "Scan"
        } else if live {
            "Scan a code"
        } else {
            "Scan from a photo"
        };
        let subtitle = if theme.text_scale >= 1.5 {
            ""
        } else if live {
            "Point at any QR or barcode — detection is automatic"
        } else {
            "No camera — pick a saved photo to scan a code from it"
        };
        Self::draw_glass_header(
            scene,
            t,
            tr,
            theme,
            Point::new(full.x0 + EDGE, full.y0 + 18.0),
            (full.width() - EDGE * 2.0 - 88.0).max(120.0),
            title,
            subtitle,
        );

        // Photos is a handoff to its owning app, never a gallery inside Camera.
        self.glass_button(
            scene,
            t,
            theme,
            Circle::new(Point::new(full.x1 - EDGE - 58.0, full.y0 + 34.0), 16.0),
            "image",
            Action::OpenPhotos,
        );
        // Settings gear, top-right.
        self.glass_button(
            scene,
            t,
            theme,
            Circle::new(
                Point::new(full.x1 - EDGE - 16.0, full.y0 + EDGE + 16.0),
                16.0,
            ),
            "settings",
            Action::Nav(Screen::Settings),
        );

        // Recent scans, floating glass panel (wide only) — below the gear and
        // clear of the bottom mode switcher.
        if wide {
            let panel = Rect::new(
                full.x1 - EDGE - panel_w,
                full.y0 + 62.0,
                full.x1 - EDGE,
                full.y1 - 74.0,
            );
            self.draw_scan_panel(scene, t, tr, theme, panel);
        }

        // Bottom-centre mode switcher: the scanner is a peer of Photo/Video, so
        // you switch back to the camera the same way you got here. Centred on
        // the feed (clear of the recent-scans panel) to align with the scan
        // window and result card.
        // Bottom edge where the 34 px capsule ended before (`y1 - 56 + 34`), so
        // 1× placement is unchanged and larger labels grow upward into the feed.
        self.draw_mode_switcher(scene, t, tr, theme, full.center().x, full.y1 - 22.0);
    }

    /// A compact floating glass header (title + subtitle) used as an overlay
    /// label on the scanner's edge-to-edge feed.
    fn draw_glass_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        pos: Point,
        max_width: f64,
        title: &str,
        subtitle: &str,
    ) {
        let colors = &theme.tokens.colors;
        let inner = (max_width - 32.0).max(40.0);
        let title = tr.elide_to_width(title, 15.0, ui::num::f32_of_f64(inner));
        let subtitle = (!subtitle.is_empty())
            .then(|| tr.elide_to_width(subtitle, 11.5, ui::num::f32_of_f64(inner)));
        let tw = f64::from(p::text_w(tr, &title, 15.0));
        let sw = subtitle
            .as_deref()
            .map_or(0.0, |value| f64::from(p::text_w(tr, value, 11.5)));
        let width = (tw.max(sw) + 32.0).min(max_width);
        let height = if subtitle.is_some() { 52.0 } else { 36.0 };
        let rect = Rect::new(pos.x, pos.y, pos.x + width, pos.y + height);
        p::fill_round(scene, t, rect, 16.0, p::alpha(colors.neutral[0], 0.55));
        p::stroke_round(
            scene,
            t,
            rect,
            16.0,
            ui::tokens::colors::Colors::hairline_on(colors.surfaces.raised, 26),
            1.0,
        );
        p::text(
            scene,
            tr,
            pos.x + 16.0,
            pos.y + 12.0,
            &title,
            15.0,
            hud_ink(theme),
        );
        if let Some(subtitle) = subtitle {
            p::text(
                scene,
                tr,
                pos.x + 16.0,
                pos.y + 32.0,
                &subtitle,
                11.5,
                p::alpha(hud_ink(theme), 0.7),
            );
        }
    }

    fn draw_scanner_feed(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        stage: Rect,
    ) {
        // The fixture/decode coordinate space is the same panel-excluded stage
        // used by the guide and result card.  This keeps the QR target centred
        // inside its guide rather than spilling into the Recent scans rail.
        self.update_scan_state(stage);
        self.paint_scan_feed(scene, t, stage);
        Self::draw_scan_window(scene, t, theme, full, stage);
        self.draw_detected_overlay(scene, t, tr, theme, stage);

        // Floating result card, centred on the frame under the scan window.
        if let Some(s) = self.scanned.clone()
            && !self.scan_dismissed
        {
            self.draw_scan_card(scene, t, tr, theme, stage, &s);
        }
    }

    /// Refresh the cached scan feed image and decode result for `full`'s
    /// current size (keyed so it only redoes the work when the size changes).
    fn update_scan_state(&mut self, full: Rect) {
        let vw = full.width().max(2.0) as u32;
        let vh = full.height().max(2.0) as u32;
        match &self.storage {
            Storage::Sample => {
                if self
                    .scan_view
                    .as_ref()
                    .is_none_or(|(w, h, _)| (*w, *h) != (vw, vh))
                {
                    let (buf, scanned) = frame::scanner_frame(4, vw, vh, SCAN_PAYLOAD);
                    self.scan_view = Some((vw, vh, frame_image_fill(buf)));
                    self.scanned = scanned;
                    self.scan_gen = (vw, vh);
                }
            }
            Storage::Disk(_) => {
                let cur_gen = self.frame_gen;
                if self.live_view.as_ref().is_none_or(|(g, _)| *g != cur_gen)
                    && let Some(fb) = self.latest_frame.clone()
                {
                    let (fw, fh) = (fb.w, fb.h);
                    self.scan_gen = (fw, fh);
                    let scanner_frame = fb.clone();
                    self.live_view = Some((cur_gen, frame_image_cover(fb)));
                    let Some(generation) = self.scan_generation.checked_add(1) else {
                        self.toast = Some("Code scan counter exhausted; restart Camera".into());
                        return;
                    };
                    let id = format!("live:{cur_gen}");
                    match self.worker.submit_scan(crate::worker::ScanJob {
                        generation,
                        id: id.clone(),
                        source: crate::worker::ScanSource::Frame(scanner_frame),
                        record_history: false,
                    }) {
                        Ok(()) => {
                            self.scan_generation = generation;
                            self.pending_scan = Some((id, generation));
                        }
                        // A frame arrives continuously. A single busy decoder
                        // is expected; keep the previous result until the next
                        // frame rather than spamming a toast.
                        Err(error) if error.contains("already running") => {}
                        Err(error) => self.toast = Some(error),
                    }
                }
            }
        }
    }

    /// Paint the edge-to-edge scan feed image into `full`.
    fn paint_scan_feed(&mut self, scene: &mut Scene, t: Affine, full: Rect) {
        match &self.storage {
            Storage::Sample => {
                if let Some((_, _, img)) = self.scan_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
            Storage::Disk(_) => {
                if let Some((_, img)) = self.live_view.as_mut() {
                    img.paint_at(scene, t, full);
                }
            }
        }
    }

    /// Darken everything but a scan window centred on the frame (where the
    /// subject — and the hermetic sample QR — actually is), with corner
    /// brackets and a centre guide line. The window side is clamped to the
    /// (panel-excluded) stage, so it stays clear of the recent-scans panel on
    /// wide layouts.
    fn draw_scan_window(scene: &mut Scene, t: Affine, theme: &Theme, full: Rect, stage: Rect) {
        let side = (stage.width().min(stage.height()) * 0.62).clamp(180.0, 460.0);
        let wc = Point::new(stage.center().x, full.center().y);
        let win = Rect::new(
            wc.x - side / 2.0,
            wc.y - side / 2.0,
            wc.x + side / 2.0,
            wc.y + side / 2.0,
        );
        let dark = p::alpha(media_scrim(theme), 0.5);
        p::fill_rect(scene, t, Rect::new(full.x0, full.y0, full.x1, win.y0), dark);
        p::fill_rect(scene, t, Rect::new(full.x0, win.y1, full.x1, full.y1), dark);
        p::fill_rect(scene, t, Rect::new(full.x0, win.y0, win.x0, win.y1), dark);
        p::fill_rect(scene, t, Rect::new(win.x1, win.y0, full.x1, win.y1), dark);
        let blen = 28.0;
        let acc = theme.accent_primary;
        for &(sx, sy) in &[(-1.0_f64, -1.0_f64), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let ox = if sx < 0.0 { win.x0 } else { win.x1 };
            let oy = if sy < 0.0 { win.y0 } else { win.y1 };
            p::line(scene, t, ox, oy, ox - sx * blen, oy, acc, 3.0);
            p::line(scene, t, ox, oy, ox, oy - sy * blen, acc, 3.0);
        }
        p::line(
            scene,
            t,
            win.x0 + 8.0,
            win.center().y,
            win.x1 - 8.0,
            win.center().y,
            p::alpha(acc, 0.7),
            2.0,
        );
    }

    /// Detected bounding box + symbology label, mapping decode-space corners
    /// into `full`'s on-screen coordinates.
    fn draw_detected_overlay(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let Some(s) = self.scanned.clone() else {
            return;
        };
        let (gw, gh) = self.scan_gen;
        let sx = full.width() / f64::from(gw.max(1));
        let sy = full.height() / f64::from(gh.max(1));
        let pt = |(x, y): (f32, f32)| {
            Point::new(full.x0 + f64::from(x) * sx, full.y0 + f64::from(y) * sy)
        };
        let mut bez = BezPath::new();
        let pts: Vec<Point> = s.corners.iter().map(|c| pt(*c)).collect();
        // `corners` is a fixed `[(f32, f32); 4]`, so `pts` always has 4 points.
        let Some((first, rest)) = pts.split_first() else {
            return;
        };
        bez.move_to(*first);
        for q in rest {
            bez.line_to(*q);
        }
        bez.close_path();
        let green = theme.status_success;
        scene.fill(
            vello::peniko::Fill::NonZero,
            t,
            p::alpha(green, 0.16),
            None,
            &bez,
        );
        scene.stroke(&vello::kurbo::Stroke::new(3.0), t, green, None, &bez);
        for q in &pts {
            p::circle(scene, t, q.x, q.y, 4.0, green);
        }
        let cxy = s.center();
        let label_text = detected_label(&s.symbology);
        let tw = f64::from(p::text_w(tr, &label_text, 11.0)) + 18.0;
        let label_x = (full.x0 + f64::from(cxy.0) * sx) - tw / 2.0;
        let top = pts.iter().map(|q| q.y).fold(f64::MAX, f64::min);
        // Authored 20 px tag at 1×; contains the measured label at any scale
        // (the 200% line used to spill 5 px past the tag's edges).
        let th = (p::text_h(tr, &label_text, 11.0) + BADGE_PAD_Y * 2.0).max(20.0);
        let label_rect = Rect::new(label_x, top - 6.0 - th, label_x + tw, top - 6.0);
        p::fill_round(scene, t, label_rect, th / 2.0, green);
        p::text_in(scene, tr, label_rect, &label_text, 11.0, hud_ink(theme));
    }

    /// No live camera: let the user scan a code out of a photo already in
    /// a portal-selected image. Real decode runs the QR detector
    /// over the file's pixels.
    fn draw_scanner_offline(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        stage: Rect,
    ) {
        let c = &theme.tokens.colors;
        p::fill_rect(scene, t, full, c.neutral[0]);
        let content = Rect::new(
            stage.x0 + EDGE,
            full.y0 + 86.0,
            stage.x1 - EDGE,
            full.y1 - 76.0,
        );
        let cx = content.center().x;
        let cy = content.center().y;
        let ic = Rect::new(cx - 28.0, cy - 92.0, cx + 28.0, cy - 36.0);
        p::icon(scene, t, "scan", ic, p::alpha(hud_ink(theme), 0.72), 2.2);
        p::text_center(
            scene,
            tr,
            cx,
            cy - 14.0,
            "Scan without a camera",
            19.0,
            hud_ink(theme),
        );
        let hint = tr.elide_to_width(
            "Choose a QR or barcode image from this device",
            12.5,
            ui::num::f32_of_f64((content.width() - 24.0).max(120.0)),
        );
        p::text_center(
            scene,
            tr,
            cx,
            cy + 16.0,
            &hint,
            12.5,
            p::alpha(hud_ink(theme), 0.68),
        );
        let button_width = (content.width() - 24.0).clamp(160.0, 240.0);
        let button = Rect::new(
            cx - button_width / 2.0,
            cy + 48.0,
            cx + button_width / 2.0,
            cy + 96.0,
        );
        p::fill_round(scene, t, button, 14.0, theme.accent_primary);
        let label = if self.scan_image_picker.is_some() {
            "Waiting for file picker…"
        } else {
            "Choose image"
        };
        p::text_center(scene, tr, cx, button.y0 + 16.0, label, 14.0, hud_ink(theme));
        if self.scan_image_picker.is_none() {
            self.push_region(button, Action::ImportScanImage);
        }

        if let Some(s) = self.scanned.clone()
            && !self.scan_dismissed
        {
            self.draw_scan_card(scene, t, tr, theme, stage, &s);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the scan result card is one responsive custom-painted semantic component"
    )]
    fn draw_scan_card(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        stage: Rect,
        s: &ScannedCode,
    ) {
        let c = &theme.tokens.colors;
        let cw = stage.width().min(560.0) - 36.0;
        let actions = s.kind.actions();
        let available = cw - 32.0;
        let large_type = theme.text_scale >= 1.5;
        // Text is scaled by the theme at paint time. Give zoomed labels a
        // larger hit target and extra horizontal breathing room rather than
        // letting the painted glyphs escape the normal-size pill.
        let button_font = if large_type { 11.0 } else { 13.0 };
        let button_padding = if large_type { 42.0 } else { 30.0 };
        let button_height = if large_type { 48.0 } else { 30.0 };
        let button_gap = 10.0;
        let widths: Vec<f64> = actions
            .iter()
            .map(|label| f64::from(p::text_w(tr, label, button_font)) + button_padding)
            .collect();
        let mut rows = 1usize;
        let mut used = 0.0;
        for width in &widths {
            if used > 0.0 && used + 10.0 + width > available {
                rows += 1;
                used = 0.0;
            }
            used += if used > 0.0 { 10.0 } else { 0.0 } + width;
        }
        let action_height =
            button_height * rows as f64 + button_gap * rows.saturating_sub(1) as f64;
        let ch = if large_type { 94.0 } else { 82.0 } + 16.0 + action_height;
        // Sit above the bottom mode switcher rather than under it.
        let bottom = stage.y1 - 76.0;
        let card = Rect::new(
            stage.center().x - cw / 2.0,
            bottom - ch,
            stage.center().x + cw / 2.0,
            bottom,
        );
        p::fill_round(scene, t, card, 18.0, p::alpha(c.neutral[0], 0.92));
        p::stroke_round(
            scene,
            t,
            card,
            18.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );

        let icon_name = code_icon(&s.kind);
        let badge = Rect::new(
            card.x0 + 16.0,
            card.y0 + 16.0,
            card.x0 + 60.0,
            card.y0 + 60.0,
        );
        p::fill_round(scene, t, badge, 13.0, p::alpha(theme.accent_primary, 0.20));
        p::icon(
            scene,
            t,
            icon_name,
            shrink(badge, 11.0),
            theme.accent_primary,
            2.0,
        );

        let tx = badge.x1 + 14.0;
        let title_size = if large_type { 13.0 } else { 15.0 };
        let summary_size = if large_type { 11.0 } else { 13.0 };
        p::text(
            scene,
            tr,
            tx,
            card.y0 + 16.0,
            s.kind.title(),
            title_size,
            hud_ink(theme),
        );
        let summ = s.kind.summary(&s.raw);
        let summ = tr.elide_to_width(
            &summ,
            summary_size,
            ui::num::f32_of_f64(card.x1 - tx - 40.0),
        );
        p::text(
            scene,
            tr,
            tx,
            card.y0 + if large_type { 50.0 } else { 39.0 },
            &summ,
            summary_size,
            p::alpha(hud_ink(theme), 0.7),
        );

        let xr = Rect::new(
            card.x1 - 34.0,
            card.y0 + 12.0,
            card.x1 - 12.0,
            card.y0 + 34.0,
        );
        p::icon(scene, t, "close", xr, p::alpha(hud_ink(theme), 0.6), 1.8);
        self.push_tap_region(xr, Action::DismissScan);

        let mut bx = card.x0 + 16.0;
        let mut by = card.y1 - 16.0 - action_height;
        for (i, (label, bw)) in actions.iter().zip(widths).enumerate() {
            if bx > card.x0 + 16.0 && bx + bw > card.x1 - 16.0 {
                bx = card.x0 + 16.0;
                by += button_height + button_gap;
            }
            let br = Rect::new(bx, by, bx + bw, by + button_height);
            if i == 0 {
                p::fill_round(scene, t, br, button_height / 2.0, theme.accent_primary);
                p::text_in(scene, tr, br, label, button_font, hud_ink(theme));
            } else {
                p::fill_round(scene, t, br, button_height / 2.0, c.lift(0.72));
                p::text_in(
                    scene,
                    tr,
                    br,
                    label,
                    button_font,
                    p::alpha(hud_ink(theme), 0.92),
                );
            }
            self.push_tap_region(br, Action::Scan(i));
            bx += bw + 10.0;
        }
    }

    fn draw_scan_panel_empty(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        let cx = panel.center().x;
        let cy = panel.y0 + 120.0;
        p::icon(
            scene,
            t,
            "scan",
            Rect::new(cx - 18.0, cy - 18.0, cx + 18.0, cy + 18.0),
            p::alpha(hud_ink(theme), 0.45),
            1.8,
        );
        // The second line follows the first's measured height (13 → 16.25
        // + 3.75 gap = the authored 20 px pitch at 1×) instead of a fixed
        // offset that 200% lines painted through.
        let title = "No codes scanned yet";
        let title_top = cy + 28.0;
        let title_h = p::text_h(tr, title, 13.0);
        p::text_center(
            scene,
            tr,
            cx,
            title_top,
            title,
            13.0,
            p::alpha(hud_ink(theme), 0.7),
        );
        p::text_center(
            scene,
            tr,
            cx,
            title_top + title_h + 3.75,
            "Scanned codes show up here",
            11.5,
            p::alpha(hud_ink(theme), 0.5),
        );
    }

    fn draw_scan_panel(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        let c = &theme.tokens.colors;
        p::fill_round(scene, t, panel, 20.0, p::alpha(c.neutral[0], 0.55));
        p::stroke_round(
            scene,
            t,
            panel,
            20.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        let px = panel.x0 + 18.0;
        p::text(
            scene,
            tr,
            px,
            panel.y0 + 20.0,
            "Recent scans",
            14.0,
            hud_ink(theme),
        );

        if self.scans.is_empty() {
            Self::draw_scan_panel_empty(scene, t, tr, theme, panel);
            return;
        }

        let now = now_secs();
        // Rows keep the authored 44 px (+2 gap) at 1× and grow to hold the
        // measured summary + timestamp block, centred, at larger text.
        let summ_h = p::text_h(tr, "Ag", 13.0);
        let when_h = p::text_h(tr, "Ag", 11.0);
        let block_h = summ_h + when_h;
        let row_h = (block_h + SCAN_ROW_PAD_Y * 2.0).max(44.0);
        let mut y = panel.y0 + 20.0 + p::text_h(tr, "Recent scans", 14.0) + 12.5;
        for entry in self.scans.clone().iter().take(8) {
            if y + row_h > panel.y1 - 12.0 {
                break;
            }
            let kind = classify(&entry.raw);
            let row = Rect::new(panel.x0 + 8.0, y, panel.x1 - 8.0, y + row_h);
            p::icon(
                scene,
                t,
                code_icon(&kind),
                Rect::new(
                    row.x0 + 8.0,
                    row.center().y - 9.0,
                    row.x0 + 26.0,
                    row.center().y + 9.0,
                ),
                p::alpha(c.accent[6], 0.95),
                1.8,
            );
            let summ = tr.elide_to_width(
                &kind.summary(&entry.raw),
                13.0,
                ui::num::f32_of_f64(row.width() - 44.0),
            );
            let summ_top = row.center().y - block_h / 2.0;
            p::text(
                scene,
                tr,
                row.x0 + 36.0,
                summ_top,
                &summ,
                13.0,
                hud_ink(theme),
            );
            p::text(
                scene,
                tr,
                row.x0 + 36.0,
                summ_top + summ_h,
                &ui::format::relative(now - entry.ts),
                11.0,
                p::alpha(hud_ink(theme), 0.6),
            );
            y += row_h + 2.0;
        }
    }
}

/// The badge shown over a live-detected code in the scanner viewfinder, e.g.
/// `"QR Code detected"`. A single space between the symbology and the verb.
fn detected_label(symbology: &str) -> String {
    format!("{symbology} detected")
}

fn code_icon(kind: &CodeKind) -> &'static str {
    match kind {
        CodeKind::Url => "link",
        CodeKind::Wifi { .. } => "wifi",
        CodeKind::Contact { .. } => "person",
        CodeKind::Email => "copy",
        CodeKind::Phone => "phone",
        CodeKind::Geo => "map",
        CodeKind::Text => "search",
    }
}

/// Scroll position/extent bundle for [`CameraApp::draw_scrollbar`].
#[derive(Clone, Copy)]
struct ScrollMetrics {
    scroll: f64,
    max_scroll: f64,
    view_h: f64,
    content_h: f64,
}

/// One description of the gallery's chrome band — the back button, the
/// "Library" title with its item count, the All/Photos/Videos/Favorites
/// filter — and where the grid or a detail panel starts beneath it.
///
/// `draw_gallery` derives it once from measured lines; the header, the filter
/// and the grid all read it. It used to be three sets of literals (title at
/// +16, count at +44, grid at +64 or +104) that agreed only at 1×: at 200% the
/// title painted over its count, the filter's fixed 80 px segments over each
/// other, and on a 480 px window the filter sat on the title's row and ran
/// into it.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg(test)]
struct GalleryChrome {
    /// Centre of the 32 px back button.
    back: Point,
    title_x: f64,
    title_top: f64,
    count_top: f64,
    /// The segmented filter capsule.
    filter: Rect,
    /// Top edge of the grid and of a docked detail panel.
    content_top: f64,
}

#[cfg(test)]
impl GalleryChrome {
    /// Authored 1× values: title 20 px at +16, count 12.5 px at +44, grid at
    /// +64 (or +104 when the filter owns the row below the title).
    const TITLE_SIZE: f32 = 20.0;
    const COUNT_SIZE: f32 = 12.5;
    const TITLE_TOP: f64 = 16.0;
    const TITLE_COUNT_GAP: f64 = 3.0;
    const HEADER_MIN_BOTTOM: f64 = 64.0;
    const FILTER_MIN_H: f64 = 32.0;
    /// Room above and below a filter label (the 16.25 px 1× line + 15 stays
    /// inside the authored 32; at 200% the capsule grows to hold the line).
    const FILTER_LABEL_PAD_Y: f64 = 7.5;
    const FILTER_MAX_W: f64 = 320.0;
    /// Room between the title and a filter sharing its row, and between the
    /// filter and the grid when the filter owns its own row.
    const GAP: f64 = 16.0;

    fn measure(
        tr: &mut TextRenderer,
        content: Rect,
        pad: f64,
        count: &str,
        filter_labels: &[&str],
    ) -> Self {
        let back = Point::new(content.x0 + pad + 16.0, content.y0 + 30.0);
        let title_x = back.x + 32.0;
        let title_top = content.y0 + Self::TITLE_TOP;
        let title_h = p::text_h(tr, "Library", Self::TITLE_SIZE);
        let title_w = f64::from(p::text_w(tr, "Library", Self::TITLE_SIZE));
        let count_top = title_top + title_h + Self::TITLE_COUNT_GAP;
        let count_h = p::text_h(tr, count, Self::COUNT_SIZE);
        // The count's line box carries its own leading (18.25 px at 1×, so
        // 44 + 18.25 = 62.25 and the authored 64 floor holds); at larger text
        // the header ends where that box ends.
        let header_bottom = (count_top + count_h).max(content.y0 + Self::HEADER_MIN_BOTTOM);

        let available = (content.width() - 2.0 * pad).max(1.0);
        let filter_w = available
            .min(Self::FILTER_MAX_W)
            .max(CameraApp::segmented_min_w(tr, filter_labels))
            .min(available);
        let label_h = filter_labels
            .iter()
            .map(|label| p::text_h(tr, label, MODE_LABEL_SIZE))
            .fold(0.0_f64, f64::max);
        let filter_h = (label_h + Self::FILTER_LABEL_PAD_Y * 2.0).max(Self::FILTER_MIN_H);
        // The filter shares the title's row only when the measured title and
        // the measured capsule both fit with a gap between them; otherwise it
        // owns the row beneath the count.
        let beside = title_x + title_w + Self::GAP + filter_w <= content.x1 - pad;
        let filter_x1 = content.x1 - pad;
        let filter = if beside {
            Rect::new(
                filter_x1 - filter_w,
                title_top,
                filter_x1,
                title_top + filter_h,
            )
        } else {
            Rect::new(
                filter_x1 - filter_w,
                header_bottom,
                filter_x1,
                header_bottom + filter_h,
            )
        };
        let content_top = if beside {
            header_bottom.max(filter.y1 + Self::GAP)
        } else {
            filter.y1 + Self::GAP
        };
        Self {
            back,
            title_x,
            title_top,
            count_top,
            filter,
            content_top,
        }
    }
}

/// Icon/label/style bundle for compact camera controls.
#[derive(Clone, Copy)]
struct PillContent<'a> {
    icon: Option<&'a str>,
    label: &'a str,
    active: bool,
}

/// Feed/recording state for the viewfinder capture-cluster helpers.
#[derive(Clone, Copy)]
struct CaptureClusterState {
    has_feed: bool,
    recording: bool,
}

/// The concise narrow label is paint-only; accessibility retains the full
/// operation name from [`CameraApp::a11y_node`].
fn recovery_button_label<'a>(action: &Action, label: &'a str, width: f64) -> &'a str {
    if matches!(action, Action::OpenCameraPrivacySettings) && width < 142.0 {
        "Privacy settings"
    } else {
        label
    }
}

// Settings screen
// ====================================================================

/// Label + value/action bundle for [`CameraApp::value_row`].
struct ValueRowContent<'a> {
    label: &'a str,
    value: &'a str,
    action: Action,
}

/// Label + state/action bundle for [`CameraApp::toggle_row`].
struct ToggleRowContent<'a> {
    label: &'a str,
    on: bool,
    action: Action,
}

impl CameraApp {
    /// The dim behind the settings sheet: the theme's scrim hue, so it
    /// recedes into the chrome's colour family over the viewfinder.
    fn settings_scrim(theme: &Theme) -> Color {
        p::alpha(theme.tokens.colors.surfaces.scrim, 0.45)
    }

    fn draw_settings(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let c = &theme.tokens.colors;

        // Camera settings are an overlay sheet, not a full desktop pane. A
        // dim scrim backs it; tapping the scrim dismisses back to the
        // viewfinder.
        p::fill_rect(scene, t, full, Self::settings_scrim(theme));
        self.push_region(full, Action::Back);

        // Centred sheet — wide enough that grouped rows use the full width and
        // stay aligned, capped so it reads as a sheet rather than a stranded
        // narrow column in an empty window.
        let large_type = theme.text_scale >= 1.5;
        let total = Self::settings_total_height(theme);
        let horizontal_margin = if full.width() < 500.0 { 24.0 } else { 96.0 };
        let sw = 620.0_f64.min(full.width() - horizontal_margin);
        let sh = (total + 24.0).min(full.height() - 72.0);
        let sx = full.center().x - sw / 2.0;
        let sy = full.center().y - sh / 2.0;
        let sheet = Rect::new(sx, sy, sx + sw, sy + sh);
        // Base-surface sheet so the raised grouped cards read distinctly on
        // it; its edge against the scrim is the Panel border (HIG §9.5).
        p::fill_round(scene, t, sheet, 22.0, c.surfaces.base);
        p::stroke_round(scene, t, sheet, 22.0, ui::chrome::panel_border(theme), 1.0);
        // Absorb taps on the sheet's own padding so only the scrim dismisses.
        self.push_region(sheet, Action::Nav(Screen::Settings));

        let inner = 22.0;
        let x0 = sheet.x0 + inner;
        let colw = sheet.width() - inner * 2.0;

        // Full painted height stays reachable via scroll on short sheets.
        let max_scroll = (total - sh).max(0.0);
        self.settings_scroll = self.settings_scroll.clamp(0.0, max_scroll);
        let off = self.settings_scroll;

        p::push_clip_round(scene, t, sheet, 22.0);
        let mut y = sheet.y0 + 22.0 - off;
        p::text(scene, tr, x0, y, "Settings", 22.0, c.text_primary);
        y += if large_type { 60.0 } else { 44.0 };

        y = self.draw_settings_capture_group(scene, t, tr, theme, x0, y, colw);
        y = self.draw_settings_composition_group(scene, t, tr, theme, x0, y, colw);
        self.draw_settings_privacy_group(scene, t, tr, theme, x0, y, colw);

        p::pop(scene);

        // Close button, top-right of the sheet — fixed above the scroll.
        let xr = Rect::new(
            sheet.x1 - 44.0,
            sheet.y0 + 14.0,
            sheet.x1 - 16.0,
            sheet.y0 + 42.0,
        );
        p::fill_round(scene, t, xr, 12.0, p::alpha(c.neutral[2], 0.9));
        p::icon(scene, t, "close", shrink(xr, 8.0), c.text_primary, 1.8);
        self.push_tap_region(xr, Action::Back);

        if max_scroll > 0.0 {
            Self::draw_scrollbar(
                scene,
                t,
                theme,
                sheet,
                ScrollMetrics {
                    scroll: off,
                    max_scroll,
                    view_h: sh,
                    content_h: total,
                },
            );
        }
    }

    /// Capture group: resolution, photo format, HDR, self-timer. Returns the
    /// y coordinate the next group should start at.
    fn draw_settings_capture_group(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        y: f64,
        colw: f64,
    ) -> f64 {
        let y = Self::group_title(scene, tr, theme, x0, y, "Capture");
        let row_height = Self::settings_row_height(theme);
        let cap_rows = 4;
        let cap_rect = Rect::new(x0, y, x0 + colw, y + f64::from(cap_rows) * row_height);
        Self::group_card(scene, t, theme, cap_rect);
        let ry = self.value_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, y, x0 + colw, y + row_height),
            ValueRowContent {
                label: "Resolution",
                value: self.settings.resolution.label(),
                action: Action::CycleResolution,
            },
        );
        Self::row_sep(scene, t, theme, x0, ry, colw);
        let ry = self.value_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, ry, x0 + colw, ry + row_height),
            ValueRowContent {
                label: "Photo format",
                value: self.settings.photo_format.label(),
                action: Action::CycleFormat,
            },
        );
        Self::row_sep(scene, t, theme, x0, ry, colw);
        let ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, ry, x0 + colw, ry + row_height),
            ToggleRowContent {
                label: "HDR",
                on: self.settings.hdr,
                action: Action::ToggleHdr,
            },
        );
        Self::row_sep(scene, t, theme, x0, ry, colw);
        let ry = self.timer_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, ry, x0 + colw, ry + row_height),
        );
        ry + 20.0
    }

    /// Composition group: grid, level guide, mirror front camera. Returns the
    /// y coordinate the next group should start at.
    fn draw_settings_composition_group(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        y: f64,
        colw: f64,
    ) -> f64 {
        let y = Self::group_title(scene, tr, theme, x0, y, "Composition");
        let row_height = Self::settings_row_height(theme);
        let comp_rect = Rect::new(x0, y, x0 + colw, y + 3.0 * row_height);
        Self::group_card(scene, t, theme, comp_rect);
        let ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, y, x0 + colw, y + row_height),
            ToggleRowContent {
                label: "Grid (rule of thirds)",
                on: self.settings.grid,
                action: Action::ToggleGrid,
            },
        );
        Self::row_sep(scene, t, theme, x0, ry, colw);
        let ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, ry, x0 + colw, ry + row_height),
            ToggleRowContent {
                label: "Level guide",
                on: self.settings.level,
                action: Action::ToggleLevel,
            },
        );
        Self::row_sep(scene, t, theme, x0, ry, colw);
        let ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, ry, x0 + colw, ry + row_height),
            ToggleRowContent {
                label: "Mirror front camera",
                on: self.settings.mirror_front,
                action: Action::ToggleMirror,
            },
        );
        ry + 20.0
    }

    /// Privacy & camera group: location tags, default camera.
    fn draw_settings_privacy_group(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x0: f64,
        y: f64,
        colw: f64,
    ) {
        let y = Self::group_title(scene, tr, theme, x0, y, "Privacy & camera");
        let row_height = Self::settings_row_height(theme);
        let pc_rect = Rect::new(x0, y, x0 + colw, y + 2.0 * row_height);
        Self::group_card(scene, t, theme, pc_rect);
        let ry = self.toggle_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, y, x0 + colw, y + row_height),
            ToggleRowContent {
                label: "Location tags",
                on: self.settings.geotag,
                action: Action::ToggleGeotag,
            },
        );
        Self::row_sep(scene, t, theme, x0, ry, colw);
        let _ = self.value_row(
            scene,
            t,
            tr,
            theme,
            Rect::new(x0, ry, x0 + colw, ry + row_height),
            ValueRowContent {
                label: "Default camera",
                value: self.settings.default_facing.label(),
                action: Action::CycleDefaultFacing,
            },
        );
    }

    /// Full painted height of the settings sheet, independent of window size.
    /// Mirrors the group layout in [`Self::draw_settings`]; the two must stay
    /// in step so scroll always reaches the last row.
    fn settings_total_height(theme: &Theme) -> f64 {
        let large_type = theme.text_scale >= 1.5;
        let row = Self::settings_row_height(theme);
        let header = if large_type { 32.0 } else { 24.0 };
        let group_gap = 20.0;
        let body = if large_type { 60.0 } else { 44.0 } // title → first group
            + header + 4.0 * row + group_gap            // Capture (4 rows)
            + header + 3.0 * row + group_gap            // Composition (3 rows)
            + header + 2.0 * row; // Privacy & camera (2 rows)
        22.0 + body + 24.0 // top gap + body + bottom breathing room
    }

    fn settings_row_height(theme: &Theme) -> f64 {
        if theme.text_scale >= 1.5 { 84.0 } else { 52.0 }
    }

    fn group_title(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        label: &str,
    ) -> f64 {
        p::text(
            scene,
            tr,
            x + 4.0,
            y,
            &label.to_uppercase(),
            11.0,
            p::alpha(theme.tokens.colors.text_dim, 0.9),
        );
        y + if theme.text_scale >= 1.5 { 32.0 } else { 24.0 }
    }

    /// A grouped settings card: raised fill with its derived rest edge
    /// (`ui::surface::card`, HIG §9.5).
    fn group_card(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect) {
        surface::card(scene, t, rect, 14.0, theme.tokens.colors.surfaces.raised);
    }

    /// The rule between two rows of a grouped card: derived against the
    /// card's surface and inset `s3` from both ends (HIG §9.5).
    fn row_sep(scene: &mut Scene, t: Affine, theme: &Theme, x: f64, y: f64, w: f64) {
        surface::separator(
            scene,
            t,
            x,
            x + w,
            y,
            SEPARATOR_INSET,
            theme.tokens.colors.surfaces.raised,
        );
    }

    /// A row with a label and a toggle switch, e.g. "Grid (rule of thirds)".
    /// `row` is the row's full bounds; returns `row.y1` so callers can chain
    /// rows without recomputing it.
    fn toggle_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        content: ToggleRowContent,
    ) -> f64 {
        let ToggleRowContent { label, on, action } = content;
        let c = &theme.tokens.colors;
        let large_type = theme.text_scale >= 1.5;
        let label = if large_type {
            tr.elide_to_width(label, 14.0, ui::num::f32_of_f64(row.width() - 36.0))
        } else {
            label.to_string()
        };
        p::text(
            scene,
            tr,
            row.x0 + 18.0,
            if large_type {
                row.y0 + 10.0
            } else {
                row.center().y - 8.0
            },
            &label,
            14.0,
            c.text_primary,
        );
        let sw = Rect::new(
            row.x1 - 18.0 - 46.0,
            if large_type {
                row.y1 - 34.0
            } else {
                row.center().y - 13.0
            },
            row.x1 - 18.0,
            if large_type {
                row.y1 - 8.0
            } else {
                row.center().y + 13.0
            },
        );
        self.toggle_switch(scene, t, theme, sw, on, action);
        row.y1
    }

    /// A row with a label and a tappable value (e.g. "Resolution: 4K"),
    /// chevron-led. `row` is the row's full bounds; returns `row.y1` so
    /// callers can chain rows without recomputing it.
    fn value_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        content: ValueRowContent,
    ) -> f64 {
        let ValueRowContent {
            label,
            value,
            action,
        } = content;
        let c = &theme.tokens.colors;
        let large_type = theme.text_scale >= 1.5;
        let label = if large_type {
            tr.elide_to_width(label, 14.0, ui::num::f32_of_f64(row.width() - 36.0))
        } else {
            label.to_string()
        };
        p::text(
            scene,
            tr,
            row.x0 + 18.0,
            if large_type {
                row.y0 + 10.0
            } else {
                row.center().y - 8.0
            },
            &label,
            14.0,
            c.text_primary,
        );
        let chev = Rect::new(
            row.x1 - 30.0,
            if large_type {
                row.y1 - 32.0
            } else {
                row.center().y - 9.0
            },
            row.x1 - 16.0,
            if large_type {
                row.y1 - 14.0
            } else {
                row.center().y + 9.0
            },
        );
        p::icon(scene, t, "chevron", chev, p::alpha(c.text_dim, 0.9), 1.8);
        p::text_right(
            scene,
            tr,
            chev.x0 - 8.0,
            if large_type {
                row.y1 - 34.0
            } else {
                row.center().y - 8.0
            },
            value,
            13.5,
            theme.accent_primary,
        );
        self.push_region(row, action);
        row.y1
    }

    /// The self-timer row: a label plus the shared segmented switcher. `row`
    /// is the row's full bounds; returns `row.y1` so callers can chain rows
    /// without recomputing it.
    fn timer_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let large_type = theme.text_scale >= 1.5;
        p::text(
            scene,
            tr,
            row.x0 + 18.0,
            if large_type {
                row.y0 + 10.0
            } else {
                row.center().y - 8.0
            },
            "Self-timer",
            14.0,
            c.text_primary,
        );
        let seg = if large_type {
            Rect::new(row.x0 + 18.0, row.y1 - 40.0, row.x1 - 18.0, row.y1 - 8.0)
        } else {
            Rect::new(
                row.x1 - 18.0 - 168.0,
                row.center().y - 15.0,
                row.x1 - 18.0,
                row.center().y + 15.0,
            )
        };
        let items: Vec<(String, bool, Action)> = Timer::ALL
            .iter()
            .map(|tm| {
                (
                    tm.label().to_string(),
                    *tm == self.settings.timer,
                    Action::SetTimer(*tm),
                )
            })
            .collect();
        self.segmented(scene, t, tr, theme, seg, items);
        row.y1
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
    use ui::headless::{Framed, viewport};

    /// Viewfinder washes and HUD ink are chrome on media: they come from the
    /// theme scrim, and the ink clears body-text contrast on it in every
    /// shipped preset (including high contrast).
    #[test]
    fn viewfinder_chrome_follows_the_theme() {
        use ui::a11y::contrast_ratio;
        use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            assert_eq!(media_scrim(&theme), theme.tokens.colors.surfaces.scrim);
            let ink = hud_ink(&theme);
            assert!(
                contrast_ratio(ink, media_scrim(&theme)) >= ON_FILL_MIN_CONTRAST,
                "{name} HUD ink {:.2}:1 on scrim",
                contrast_ratio(ink, media_scrim(&theme))
            );
        }
    }

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

    #[test]
    fn the_settings_scrim_takes_the_theme_hue_and_still_dims_the_viewfinder() {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let scrim = CameraApp::settings_scrim(&theme);
            let lum = ui::a11y::relative_luminance(scrim.with_alpha(1.0));
            assert!(lum < 0.05, "{name}: settings scrim is not a dim ({lum:.3})");
            assert!(
                (1..255).contains(&scrim.to_rgba8().a),
                "{name}: scrim must stay translucent"
            );
        }
        assert_ne!(
            CameraApp::settings_scrim(&Theme::named("Nord").expect("Nord")),
            CameraApp::settings_scrim(&Theme::dark()),
            "the scrim follows the theme rather than a fixed black"
        );
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

    fn wheel(dy: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, dy),
            phase: winit::event::TouchPhase::Moved,
        }
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

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the segment width is computed by the same expression from the same measurement, so it must match exactly; the stage geometry pins authored whole-pixel constants, which are exact in binary floating point"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn viewfinder_layout_holds_its_controls_at_every_size_and_text_scale() {
        // Phase `mode_switcher_segments_hold_their_measured_labels_at_2x`.
        // 2026-09-17 review: the Photo / Video / Scan capsule split its authored
        // 240 px into three 80 px segments at every scale, so 200% labels ran
        // into each other. The capsule keeps 240 at 1× and takes the measured
        // labels' width (plus padding) when they need more.
        {
            let mut tr = TextRenderer::new();
            assert_eq!(
                CameraApp::segmented_min_w(&mut tr, &["Photo", "Video", "Scan"]),
                3.0 * (f64::from(p::text_w(&mut tr, "Photo", MODE_LABEL_SIZE))
                    + 2.0 * MODE_LABEL_PAD_X),
                "the widest label sizes every segment"
            );
            for scale in [1.0_f32, 2.0] {
                let (app, doc, _root) = sample_app();
                let app = render_at_scaled(app, doc, 800.0, 900.0, scale);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let tree = app.accessibility().expect("camera has a semantic window");
                let segment = |target: &str| -> Rect {
                    tree.children
                        .iter()
                        .find(|node| node.action_target.as_deref() == Some(target))
                        .and_then(|node| node.bounds)
                        .map_or_else(
                            || panic!("{target} is a painted segment"),
                            ui::a11y::AccessBounds::to_rect,
                        )
                };
                let photo = segment("camera.mode:photo");
                let video = segment("camera.mode:video");
                let scan = segment("camera.nav:scanner");
                // Tap regions grow narrow segments to the 44 px touch minimum
                // around their centre, so widths are read from adjacent centres.
                let seg_w = video.center().x - photo.center().x;
                assert!(
                    (scan.center().x - video.center().x - seg_w).abs() < 0.5,
                    "{scale}×: equal segments {photo:?} {video:?} {scan:?}"
                );
                if (scale - 1.0).abs() < f32::EPSILON {
                    let labels = ["Photo", "Video", "Codes", "Docs"];
                    let expected = 240.0_f64.max(CameraApp::segmented_min_w(&mut tr, &labels))
                        / labels.len() as f64;
                    assert!(
                        (seg_w - expected).abs() < 0.5,
                        "1× keeps the authored width unless its four labels need more: {seg_w}"
                    );
                } else {
                    let widest = ["Photo", "Video", "Scan"]
                        .iter()
                        .map(|label| f64::from(p::text_w(&mut tr, label, MODE_LABEL_SIZE)))
                        .fold(0.0_f64, f64::max);
                    assert!(
                        seg_w >= widest + 2.0 * MODE_LABEL_PAD_X,
                        "{scale}×: {seg_w} px segments hold a {widest} px label with padding"
                    );
                }
            }
        }
        // Phase `viewfinder_keeps_the_full_stage_at_wide_narrow_and_large_text_sizes`.
        {
            let (app, mut doc, _root) = sample_app();
            let heads = doc.get_heads();
            let mut app = render_at(app, doc, 1_100.0, 720.0);
            assert_eq!(app.rect.width(), 1_100.0);
            assert!(
                app.accessibility()
                    .expect("wide a11y")
                    .children
                    .iter()
                    .any(|node| { node.action_target.as_deref() == Some("camera.open-photos") })
            );

            rerender_at(&mut app, 390.0, 720.0);
            assert_eq!(app.rect.width(), 390.0);
            assert!(
                app.accessibility()
                    .expect("narrow a11y")
                    .children
                    .iter()
                    .any(|node| { node.action_target.as_deref() == Some("camera.shutter") })
            );
            assert_eq!(
                Framed::split(&mut app).1.doc.get_heads(),
                heads,
                "viewfinder layout remains local"
            );
        }
        // Phase `viewfinder_capsules_contain_their_labels_at_accessibility_scale`.
        // corrected-review-1 (2026-09-15) camera P2: at 200% text the "12 MP /
        // HDR / Timer" labels ran below their 30 px pills and "Photo" below its
        // selected segment. Capsules now grow from the measured label line, so
        // every painted control must contain its label at 1× and 2×, and the
        // four floating corner controls must sit on one edge inset.
        {
            // Tap regions are the painted rect grown to the 44 px touch minimum
            // around its centre (`push_tap_region`): anything taller than 44 is
            // the painted capsule itself, and centres are always painted centres.
            const TOUCH_MIN: f64 = 44.0;
            for scale in [1.0_f32, 2.0] {
                let (app, doc, _root) = sample_app();
                let app = render_at_scaled(app, doc, 1366.0, 768.0, scale);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let pill_label_h = f64::from(tr.measure("HDR", CTRL_PILL_LABEL_SIZE, None).1);
                let mode_label_h = f64::from(tr.measure("Photo", MODE_LABEL_SIZE, None).1);
                let tree = app.accessibility().expect("camera has a semantic window");
                let bounds_of = |target: &str| -> Rect {
                    tree.children
                        .iter()
                        .find(|node| node.action_target.as_deref() == Some(target))
                        .and_then(|node| node.bounds)
                        .map_or_else(
                            || panic!("{target} is a painted control"),
                            ui::a11y::AccessBounds::to_rect,
                        )
                };
                let photo = bounds_of("camera.mode:photo");
                let shutter = bounds_of("camera.shutter");
                let pill = bounds_of("camera.resolution");
                if (scale - 1.0).abs() < f32::EPSILON {
                    // 1× keeps the authored 30 / 34 px capsules (both under the
                    // touch minimum, so their regions read exactly 44) and the
                    // 34 px switcher ends `ACTION_GAP` above the shutter's top
                    // edge, so the two 44 px tap boxes never overlap (HIG §7).
                    assert!((pill.height() - TOUCH_MIN).abs() < 0.5, "1× pill: {pill:?}");
                    assert!(
                        (photo.height() - TOUCH_MIN).abs() < 0.5,
                        "1× segment: {photo:?}"
                    );
                    assert!(
                        (photo.center().y + MODE_SWITCHER_MIN_H / 2.0 + ACTION_GAP - shutter.y0)
                            .abs()
                            < 0.5,
                        "1×: switcher centre {photo:?} puts its bottom `s3` above the shutter {shutter:?}"
                    );
                    assert!(
                        shutter.y0 - photo.y1 >= step(Spacing::BASE.s1),
                        "1×: the switcher's tap box {photo:?} keeps a token gap to the shutter's {shutter:?}"
                    );
                } else {
                    // 2×: the measured label lines exceed the minima, so the grown
                    // capsules are what the regions report — and they contain the
                    // labels with their padding, bottom edge still on the shutter.
                    let target = "camera.resolution";
                    let pill = bounds_of(target);
                    assert!(
                        pill.height() + 0.5 >= pill_label_h + CTRL_PILL_PAD_Y * 2.0,
                        "2×: {target} pill {pill:?} cannot contain a {pill_label_h} px label"
                    );
                    assert!(
                        photo.height() + 0.5 >= mode_label_h + MODE_LABEL_PAD_Y * 2.0,
                        "2×: mode segment {photo:?} cannot contain a {mode_label_h} px label"
                    );
                    assert!(
                        (shutter.y0 - photo.y1 - ACTION_GAP).abs() < 0.5,
                        "2×: switcher {photo:?} is bottom-anchored `s3` above the shutter {shutter:?}"
                    );
                }
                // Corner controls share one edge inset (checked through their
                // painted centres, which touch inflation never moves).
                let photos = bounds_of("camera.open-photos");
                let lens = bounds_of("camera.facing");
                let settings = bounds_of("camera.nav:settings");
                assert!(
                    (photos.center().x - (app.rect.x1 - EDGE - 58.0)).abs() < 0.5,
                    "Photos handoff on the top control row: {photos:?}"
                );
                assert!(
                    (lens.center().x - (app.rect.x1 - EDGE - 26.0)).abs() < 0.5,
                    "lens switch (r 26) on EDGE: {lens:?}"
                );
                assert!(
                    (settings.center().x - (app.rect.x1 - EDGE - 16.0)).abs() < 0.5,
                    "settings (r 16) on EDGE: {settings:?}"
                );
            }
        }
        // Phase `large_text_mode_switcher_keeps_each_destination_separate`.
        {
            let (app, doc, _root) = sample_app();
            let app = render_with_theme(app, doc, 390.0, 844.0, Theme::dark().with_text_scale(2.0));

            let tree = app.accessibility().expect("camera accessibility tree");
            let bounds_for = |target| {
                tree.children
                    .iter()
                    .find(|node| node.action_target.as_deref() == Some(target))
                    .and_then(|node| node.bounds)
                    .expect("mode-switcher destination is exposed")
            };
            let photo = bounds_for("camera.mode:photo");
            let video = bounds_for("camera.mode:video");
            let codes = bounds_for("camera.nav:scanner");
            let documents = bounds_for("camera.nav:document");

            assert!(
                photo.y1 - photo.y0 >= 64.0,
                "200% text gets a full-height segmented touch target"
            );
            assert!(photo.x1 <= video.x0);
            assert!(video.x1 <= codes.x0);
            assert!(codes.x1 <= documents.x0);
            assert!(documents.x1 <= app.rect.x1);
        }
        // Phase `compact_mode_switcher_stays_within_the_viewport`.
        {
            let (app, doc, _root) = sample_app();
            let app = render_at(app, doc, 260.0, 700.0);
            let tree = app.accessibility().expect("camera has a semantic window");
            let mode_controls: Vec<_> = tree
                .children
                .iter()
                .filter(|node| {
                    node.role == Role::RadioButton
                        && node
                            .action_target
                            .as_deref()
                            .is_some_and(|target| target.starts_with("camera.mode:"))
                })
                .collect();
            assert_eq!(mode_controls.len(), 2, "Photo and Video are reachable");
            assert!(mode_controls.iter().all(|node| {
                let bounds = node.bounds.expect("painted control has bounds");
                bounds.x0 >= app.rect.x0 && bounds.x1 <= app.rect.x1
            }));
        }
        // Phase `resize_reflows_the_same_instance`.
        {
            // The breakpoint must be re-derived from the current width every frame:
            // one instance rendered wide THEN narrow must reflow, not stay frozen
            // at the first frame's layout.
            let app = CameraApp::with(Storage::Sample, Screen::Scanner);
            let doc = AutoCommit::new();

            let mut app = render_at(app, doc, 1280.0, 720.0);
            assert_eq!(app.screen, Screen::Scanner);
            let wide_w = app.scan_view.as_ref().expect("wide viewfinder").0;
            assert!(app.rect.width() >= 1279.0);

            rerender_at(&mut app, 560.0, 720.0);
            let narrow_w = app.scan_view.as_ref().expect("narrow viewfinder").0;
            assert!(app.rect.width() <= 561.0, "rect tracked the resize");

            // The viewfinder image was rebuilt for the narrower width — proof the
            // layout followed the resize rather than caching the first frame.
            assert!(
                narrow_w < wide_w,
                "viewfinder reflowed {wide_w} -> {narrow_w}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the geometry pins authored whole-pixel constants, which are exact in binary floating point"
    )]
    fn gallery_header_and_settings_sheet_stay_reachable_at_narrow_and_large_text() {
        // Phase `gallery_chrome_keeps_title_count_filter_and_grid_apart`.
        // The gallery header used three literal sets that agreed only at 1×:
        // the title over its count at 200%, fixed 80 px filter segments over
        // each other, and on a 480 px window the filter on the title's row
        // running into it. One measured description now places all of it.
        {
            let labels = vec!["All", "Photos", "Videos", "Favorites"];
            // 1× desktop: the authored geometry, filter beside the title.
            let mut tr = TextRenderer::new();
            let wide = Rect::new(0.0, 0.0, 1200.0, 800.0);
            let one = GalleryChrome::measure(&mut tr, wide, 24.0, "9 items", &labels);
            assert_eq!(one.title_top, 16.0, "{one:?}");
            assert_eq!(one.count_top, 44.0, "{one:?}");
            assert_eq!(
                one.content_top,
                64.0,
                "{one:?} (count line {})",
                p::text_h(&mut tr, "9 items", GalleryChrome::COUNT_SIZE)
            );
            assert_eq!(one.filter.y0, 16.0, "1× filter shares the title row");
            assert_eq!(one.filter.height(), 32.0);
            assert_eq!(one.filter.width(), 320.0);
            assert_eq!(one.filter.x1, wide.x1 - 24.0);

            // 480 px: the filter cannot share the title's row without touching
            // it, so it owns the row under the count and the grid follows it.
            let phone = Rect::new(0.0, 0.0, 480.0, 900.0);
            let narrow = GalleryChrome::measure(&mut tr, phone, 24.0, "9 items", &labels);
            let count_bottom =
                narrow.count_top + p::text_h(&mut tr, "9 items", GalleryChrome::COUNT_SIZE);
            assert!(
                narrow.filter.y0 >= count_bottom,
                "phone filter {:?} starts below the count line ending at {count_bottom}",
                narrow.filter
            );
            assert!(narrow.content_top >= narrow.filter.y1 + GalleryChrome::GAP);

            // 2×: the count clears the measured title, every filter segment
            // holds the widest measured label, and the grid clears both.
            tr.set_text_scale(2.0);
            let two = GalleryChrome::measure(&mut tr, wide, 24.0, "9 items", &labels);
            let title_h = p::text_h(&mut tr, "Library", GalleryChrome::TITLE_SIZE);
            assert!(
                two.count_top >= two.title_top + title_h,
                "2× count below the title: {two:?}"
            );
            let widest = labels
                .iter()
                .map(|label| f64::from(p::text_w(&mut tr, label, MODE_LABEL_SIZE)))
                .fold(0.0_f64, f64::max);
            assert!(
                two.filter.width() / labels.len() as f64 >= widest + 2.0 * MODE_LABEL_PAD_X,
                "2× filter segments hold {widest} px labels: {:?}",
                two.filter
            );
            let count_bottom =
                two.count_top + p::text_h(&mut tr, "9 items", GalleryChrome::COUNT_SIZE);
            assert!(
                two.content_top >= count_bottom && two.content_top >= two.filter.y1,
                "2× grid starts below the header and the filter: {two:?}"
            );
        }
        // Phase `settings_sheet_needs_scroll_on_short_windows`.
        {
            // On a short window the whole sheet must remain reachable: the max
            // scroll equals how far the content overflows the viewport.
            let total = CameraApp::settings_total_height(&Theme::dark());
            assert!(
                (total - 670.0).abs() < 1.0,
                "settings height ~670, got {total}"
            );
            let short = 560.0;
            assert!(total - short > 100.0, "last rows are below a 560px fold");
            let large_type = CameraApp::settings_total_height(&Theme::dark().with_text_scale(2.0));
            assert!(
                large_type > total + 250.0,
                "large text rows need independent vertical controls"
            );
        }
        // Phase `wheel_scrolls_settings`.
        {
            let (mut app, mut doc, root) = sample_app();
            app.apply(Action::Nav(Screen::Settings), &mut doc, &root);
            assert!(drive(&mut app, &wheel(-2.0), &mut doc, &root));
            assert!(app.settings_scroll > 0.0);
        }
    }

    #[test]
    fn semantic_controls_are_plainly_named_and_dispatch_camera_actions() {
        // Phase `detected_label_has_single_space`.
        {
            let tag = detected_label("QR Code");
            assert_eq!(tag, "QR Code detected");
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
        // Phase `unavailable_camera_recovery_is_semantic_and_has_touch_targets`.
        {
            let dir = std::env::temp_dir().join("camera-recovery-test-no-device");
            let mut app = CameraApp::with(Storage::Disk(dir), Screen::Camera);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            app.camera_issue = Some(CameraIssue::PermissionDenied);
            let mut app = render_at(app, doc, 320.0, 680.0);
            let tree = app.accessibility().expect("camera has semantics");
            let privacy = tree
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Open camera privacy settings"))
                .expect("privacy recovery action");
            let refresh = tree
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Refresh available cameras"))
                .expect("refresh recovery action");
            for node in [privacy, refresh] {
                let bounds = node.bounds.expect("recovery has bounds");
                assert!(bounds.x1 - bounds.x0 >= 44.0 && bounds.y1 - bounds.y0 >= 44.0);
            }
            let (camera, state) = Framed::split(&mut app);
            assert!(camera.perform_semantic_action(
                "camera.retry-camera",
                &A11yAction::Activate,
                &mut state.doc,
                &root,
            ));
            // Device discovery is deliberately real. The test host may expose a
            // `/dev/video*` node (for example, a CI virtual camera), so assert
            // the honest result for the discovered state rather than pretending
            // every machine has no hardware.
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
            // camera and private imagery.  An explicit permission result must
            // still win over that procedural feed, otherwise visual regressions
            // would accidentally hide the most important recovery state.
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
        }
        // Phase `narrow_privacy_recovery_label_fits_a_two_action_row`.
        {
            assert_eq!(
                recovery_button_label(
                    &Action::OpenCameraPrivacySettings,
                    "Open privacy settings",
                    117.0
                ),
                "Privacy settings"
            );
            assert_eq!(
                recovery_button_label(
                    &Action::OpenCameraPrivacySettings,
                    "Open privacy settings",
                    160.0
                ),
                "Open privacy settings"
            );
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
                Some("Recognized 0 words · Straightened 2.5°")
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
                "1 word recognized · Straightened 1.5°"
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
            app.document_choice_anchors = [Rect::new(10.0, 10.0, 140.0, 54.0); 5];

            for (choice, selection) in [
                (DocumentChoice::Scanner, 1),
                (DocumentChoice::Resolution, 2),
                (DocumentChoice::Color, 1),
                (DocumentChoice::Source, 1),
                (DocumentChoice::Language, 1),
            ] {
                app.apply(Action::OpenDocumentChoice(choice), &mut doc, &root);
                assert!(app.document_popover.is_open());
                app.document_popover.select(selection);
                let selected = app.document_popover.take_selection().unwrap();
                app.apply_document_choice(selected);
            }

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

            assert_eq!(
                app.document_popover.items(),
                &["English", "German", "French", "Spanish"]
            );
        }
        // Phase `touch_activates_document_choices_on_release_and_cancels_after_dragging_off`.
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
            fn dispatch(
                app: &mut CameraApp,
                event: &WindowEvent,
                doc: &mut AutoCommit,
                node: NodeId,
                tree: &TaffyTree<()>,
            ) {
                let mut handled = false;
                let mut focus = None;
                app.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: doc,
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: tree,
                    },
                );
                assert!(handled);
            }

            let (mut app, mut doc, _root) = sample_app();
            let rect = Rect::new(20.0, 20.0, 180.0, 64.0);
            app.document_choice_anchors[DocumentChoice::Resolution.index()] = rect;
            app.regions
                .push((rect, Action::OpenDocumentChoice(DocumentChoice::Resolution)));
            let mut tree = TaffyTree::<()>::new();
            let node = tree.new_leaf(Style::default()).unwrap();

            dispatch(
                &mut app,
                &touch(TouchPhase::Started, rect.center()),
                &mut doc,
                node,
                &tree,
            );
            assert!(
                !app.document_popover.is_open(),
                "touch-down is not activation"
            );
            dispatch(
                &mut app,
                &touch(TouchPhase::Ended, rect.center()),
                &mut doc,
                node,
                &tree,
            );
            assert!(app.document_popover.is_open());

            app.document_popover.close();
            dispatch(
                &mut app,
                &touch(TouchPhase::Started, rect.center()),
                &mut doc,
                node,
                &tree,
            );
            dispatch(
                &mut app,
                &touch(
                    TouchPhase::Moved,
                    Point::new(rect.x1 + 40.0, rect.y1 + 40.0),
                ),
                &mut doc,
                node,
                &tree,
            );
            dispatch(
                &mut app,
                &touch(TouchPhase::Ended, rect.center()),
                &mut doc,
                node,
                &tree,
            );
            assert!(
                !app.document_popover.is_open(),
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
            // ("Connect") copies the password — the parse+copy path, end to end.
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
                    assert_eq!(kind.actions()[0], "Connect");
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
            app.apply(Action::Scan(0), &mut doc, &root); // "Connect"
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
