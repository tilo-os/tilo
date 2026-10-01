//! Camera's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! The camera's *pixels* are verified content-store bytes with an optional local file
//! projection — a shutter press writes a PNG/JPEG, a recording writes a clip
//! — and the *device I/O* around them
//! (opening a lens, decoding a QR out of a frame, the live viewfinder) is
//! external, not collaborative. What lives in the shared Automerge document
//! (see [`crate::doc`]) is the *metadata*: portable capture registrations,
//! mode and capture preferences, and scan history. Those durable
//! choices are [`CameraAction`]s: small serializable values that
//! deterministically mutate the CRDT and therefore replicate, replay and undo
//! for free.
//!
//! ## What is *not* an action
//!
//! Capturing a photo/video (the frame write is device I/O), media-library
//! management (owned by Photos or Video Player), and the live preview are
//! external effects. The one recent handoff, zoom, and an open chooser are
//! presentation-only.
//!
//! ## Determinism contract
//!
//! Variants carry everything [`Action::apply`] needs (the item id, the scanned
//! payload, and a timestamp minted at authoring time); `apply` never touches
//! the RNG or the wall-clock. A malformed payload is an explicit
//! [`ActionError::Refused`], so the invoker never mistakes a rejected durable
//! mutation for success.

use automerge::{AutoCommit, ROOT};
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource,
};

use crate::doc;
use crate::model::{
    CaptureKind, CaptureMode, Facing, PhotoFormat, Resolution, Screen, Settings, Timer,
};

/// Registry `type_id` for every Camera action.
pub const CAMERA_ACTION_TYPE: &str = "camera.action";
pub const CAMERA_OPERATION_TYPE: &str = "camera.operation";
pub const APPLICATION_LAUNCH_TYPE: &str = "tilo.application.launch";
/// Device discovery is external and potentially hostile. Publishing a small,
/// deterministic prefix keeps resource/search materialization bounded.
pub const MAX_PUBLISHED_CAMERA_DEVICES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraOperation {
    /// Reveal a view locally. Navigation is presentation state and must not
    /// create collaboration history or move a peer away from their work.
    Reveal {
        screen: Screen,
    },
    Capture,
    ToggleRecording,
    /// Drop a stale device session and re-run real device discovery.
    RetryCamera,
    /// Hand the system camera-privacy URI to the registered desktop handler.
    OpenCameraPrivacySettings,
    /// Select one device that was discovered in the current refresh.
    SelectCamera {
        device: String,
    },
    /// Choose an existing image through the system portal and scan it without
    /// importing or browsing a Camera-owned media library.
    ImportCodeImage,
    CaptureDocumentPage,
    RotateDocumentPage,
    RecognizeDocumentPage,
    CopyDocumentText,
    ExportDocument,
    BeginDocumentCrop,
    MoveDocumentPageEarlier,
    MoveDocumentPageLater,
    RemoveDocumentPage,
}

/// Decode one Camera effect payload, declining operation types owned by other
/// widgets.
///
/// # Errors
///
/// Returns an error when a payload claiming to be a Camera operation is not
/// valid JSON for [`CameraOperation`].
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<CameraOperation>, String> {
    if type_id != CAMERA_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Every collaborative operation Camera exports on its shared capture/scan
/// metadata. The pixels and the decode are external; these are the doc writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraAction {
    /// Atomically publish a bounded capture metadata row pointing only at
    /// verified content-store bytes. This is the peer-replayable capture boundary.
    RegisterCapture {
        id: String,
        media_id: String,
        kind: CaptureKind,
        ts: i64,
        w: u32,
        h: u32,
        dur_ms: i64,
        label: String,
        facing: Facing,
    },
    SetMode {
        mode: CaptureMode,
    },
    SetFacing {
        facing: Facing,
    },
    /// One scalar capture preference.  These stay deliberately granular: a
    /// collaborator changing the grid must not overwrite a concurrently
    /// selected timer or resolution.
    SetResolution {
        resolution: Resolution,
    },
    SetPhotoFormat {
        photo_format: PhotoFormat,
    },
    SetTimer {
        timer: Timer,
    },
    SetGrid {
        on: bool,
    },
    SetLevel {
        on: bool,
    },
    SetMirrorFront {
        on: bool,
    },
    SetGeotag {
        on: bool,
    },
    SetHdr {
        on: bool,
    },
    SetDefaultFacing {
        facing: Facing,
    },
    /// Record a decoded code into the shared scan history. The QR/barcode
    /// decode is device I/O (external); this metadata row-insert is the
    /// replayable action. `ts` is minted at authoring time and carried here so
    /// `apply` stays pure; the insert deduplicates and caps deterministically
    /// against the replay base.
    RecordScan {
        raw: String,
        ts: i64,
    },
}

impl Action for CameraAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CameraAction::RegisterCapture {
                id,
                media_id,
                kind,
                ts,
                w,
                h,
                dur_ms,
                label,
                facing,
            } => doc::register_recent_capture(
                doc,
                &ROOT,
                &doc::SharedCapture {
                    id: id.clone(),
                    media_id: media_id.clone(),
                    kind: *kind,
                    ts: *ts,
                    w: *w,
                    h: *h,
                    dur_ms: *dur_ms,
                    label: label.clone(),
                    facing: *facing,
                },
            )
            .map_err(ActionError::Refused)?,
            CameraAction::SetMode { mode } => doc::write_mode(doc, &ROOT, *mode),
            CameraAction::SetFacing { facing } => doc::write_facing(doc, &ROOT, *facing),
            CameraAction::SetResolution { resolution } => {
                doc::set_resolution(doc, &ROOT, *resolution);
            }
            CameraAction::SetPhotoFormat { photo_format } => {
                doc::set_photo_format(doc, &ROOT, *photo_format);
            }
            CameraAction::SetTimer { timer } => doc::set_timer(doc, &ROOT, *timer),
            CameraAction::SetGrid { on } => doc::set_grid(doc, &ROOT, *on),
            CameraAction::SetLevel { on } => doc::set_level(doc, &ROOT, *on),
            CameraAction::SetMirrorFront { on } => doc::set_mirror_front(doc, &ROOT, *on),
            CameraAction::SetGeotag { on } => doc::set_geotag(doc, &ROOT, *on),
            CameraAction::SetHdr { on } => doc::set_hdr(doc, &ROOT, *on),
            CameraAction::SetDefaultFacing { facing } => {
                doc::set_default_facing(doc, &ROOT, *facing);
            }
            CameraAction::RecordScan { raw, ts } => {
                doc::record_scan(doc, &ROOT, raw, *ts).map_err(ActionError::Refused)?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Camera's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<CameraAction>(CAMERA_ACTION_TYPE);
}

/// Encode any [`CameraAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Camera action into
/// a replayable envelope.
///
/// # Errors
///
/// Returns an error if `action` cannot be serialized, which does not happen
/// for the plain string/bool/i64 fields [`CameraAction`] carries today.
pub fn encode(action: &CameraAction) -> Result<EncodedAction, ActionError> {
    EncodedAction::encode(CAMERA_ACTION_TYPE, action)
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn record_scan_action(raw: &str, ts: i64) -> CameraAction {
    CameraAction::RecordScan {
        raw: raw.to_string(),
        ts,
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One recent capture surfaced as a handoff resource: its id (file name), a
/// one-line label, and whether it is a video. Everything
/// here is derived from state the app already holds; nothing is fabricated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureItem {
    pub id: String,
    pub label: String,
    pub is_video: bool,
    /// A verified portable still-image identity, when the bytes have been
    /// published. It is intentionally not a local path.
    pub media_id: Option<String>,
}

/// The compact presentation context Camera hands [`provide_resources`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this is a compact immutable availability snapshot for typed resources"
)]
pub struct CameraResourceCtx {
    pub recent: Option<CaptureItem>,
    pub mode: CaptureMode,
    pub facing: Facing,
    pub recording: bool,
    /// Device effects are only actionable when a real (or hermetic sample)
    /// viewfinder feed is available. Keeping this in the resource context
    /// means Ctrl+K tells an agent the truth before it attempts an operation.
    pub feed_available: bool,
    pub camera_permission_denied: bool,
    /// Current device nodes, real discovery only. Empty is not an error and
    /// never causes the chooser to be advertised.
    pub camera_devices: Vec<String>,
    pub selected_camera: Option<String>,
    pub settings: Settings,
    pub scan_import_pending: bool,
    pub document_scanner_available: bool,
    pub document_pages: usize,
    pub document_selected: usize,
    pub document_busy: bool,
    pub document_has_text: bool,
}

impl Default for CameraResourceCtx {
    fn default() -> Self {
        Self {
            recent: None,
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            recording: false,
            feed_available: false,
            camera_permission_denied: false,
            camera_devices: Vec::new(),
            selected_camera: None,
            settings: Settings::default(),
            scan_import_pending: false,
            document_scanner_available: false,
            document_pages: 0,
            document_selected: 0,
            document_busy: false,
            document_has_text: false,
        }
    }
}

/// Builds the palette spec for `action`, or `None` if it fails to encode (it
/// does not for today's plain string/bool/i64 `CameraAction` fields) — a
/// resource simply drops the action rather than crashing the palette.
fn immediate_spec(id: &str, label: &str, action: &CameraAction) -> Option<ActionSpec> {
    ActionSpec::immediate(id, label, CAMERA_ACTION_TYPE, action)
        .ok()
        .map(ActionSpec::reversible)
}

fn item_label(it: &CaptureItem) -> String {
    let l = it.label.trim();
    let base = if l.is_empty() { it.id.as_str() } else { l };
    if it.is_video {
        format!("{base} (video)")
    } else {
        base.to_string()
    }
}

/// The owning application for a contextual capture handoff. Camera produces
/// media, but deliberately does not grow a second collection browser around
/// it. Keeping this mapping beside the typed resource action makes the visible
/// handoff, palette action, and automation target use the same boundary.
pub(crate) const fn capture_destination(is_video: bool) -> (&'static str, &'static str) {
    if is_video {
        ("so.tilo.video-player", "Video Player")
    } else {
        ("so.tilo.photos", "Photos")
    }
}

/// Canonical application-launch action for browsing the still-image library.
///
/// Camera is deliberately allowed to *route* to Photos, but never to project
/// Photos' collection into a second, stale gallery of its own.  Keeping the
/// action here lets the painted handoff, global search, agents,
/// and history all address the exact same external operation.
pub fn open_photos_action() -> ActionSpec {
    ActionSpec::typed_operation(
        "open-photos",
        "Open Photos",
        "Browse your photo library in Photos",
        OperationKind::Effect,
        APPLICATION_LAUNCH_TYPE,
        Vec::new(),
        |_| {
            EncodedAction::encode_operation(
                APPLICATION_LAUNCH_TYPE,
                &serde_json::json!({ "app_id": "so.tilo.photos" }),
            )
            .unwrap_or_else(|_| EncodedAction {
                type_id: APPLICATION_LAUNCH_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
}

pub(crate) fn capture_resource_id(it: &CaptureItem) -> String {
    format!(
        "{}:{}",
        if it.is_video {
            "camera.video"
        } else {
            "camera.photo"
        },
        it.id
    )
}

pub(crate) fn capture_resource(it: &CaptureItem) -> Resource {
    let kind = if it.is_video {
        "camera.video"
    } else {
        "camera.photo"
    };
    let mut child = Resource::new(capture_resource_id(it), kind, item_label(it));
    if let Some(content_hash) = it
        .media_id
        .as_deref()
        .filter(|id| crate::media::is_media_id(id))
        .and_then(|id| id.strip_prefix(crate::media::MEDIA_PREFIX))
    {
        let (app_id, destination) = capture_destination(it.is_video);
        let label = format!("Open in {destination}");
        let app_id = app_id.to_owned();
        let content_hash = content_hash.to_owned();
        let spec = ActionSpec::typed_operation(
            "open",
            &label,
            "Open the verified portable capture in its owning app",
            OperationKind::Effect,
            APPLICATION_LAUNCH_TYPE,
            Vec::new(),
            move |_| {
                EncodedAction::encode_operation(
                    APPLICATION_LAUNCH_TYPE,
                    &serde_json::json!({
                        "app_id": app_id,
                        "content_hash": content_hash,
                    }),
                )
                .unwrap_or_else(|_| EncodedAction {
                    type_id: APPLICATION_LAUNCH_TYPE.into(),
                    payload: Vec::new(),
                })
            },
        );
        child = child.with_action(spec);
    }
    child
}

pub(crate) fn capture_item_from_shared(shared: &doc::SharedCapture) -> CaptureItem {
    CaptureItem {
        id: shared.id.clone(),
        label: shared.label.clone(),
        is_video: matches!(shared.kind, CaptureKind::Video),
        media_id: Some(shared.media_id.clone()).filter(|id| !id.is_empty()),
    }
}

/// Screen reveal Views for [`crate::resource::Camera::local_operations`].
pub(crate) fn screen_view_specs() -> Vec<ActionSpec> {
    let mut specs = Vec::new();
    for screen in Screen::ALL {
        if let Ok(spec) = ActionSpec::operation(
            format!("open-{}", screen.as_str()),
            format!("Open {}", screen.label()),
            OperationKind::View,
            CAMERA_OPERATION_TYPE,
            &CameraOperation::Reveal { screen },
        ) {
            specs.push(spec);
        }
    }
    specs
}

/// Mode / lens Document actions bound from live capture state.
pub(crate) fn bound_mode_facing_actions(facing: Facing, recording: bool) -> Vec<ActionSpec> {
    let mut specs = Vec::new();
    for (id, label, mode) in [
        ("photo-mode", "Use photo mode", CaptureMode::Photo),
        ("video-mode", "Use video mode", CaptureMode::Video),
    ] {
        if let Some(spec) = immediate_spec(id, label, &CameraAction::SetMode { mode }) {
            specs.push(if recording {
                spec.unavailable("recording_active", "Stop recording before changing mode")
            } else {
                spec
            });
        }
    }
    for next in Facing::ALL {
        if let Some(spec) = immediate_spec(
            &format!("lens-{}", next.as_str()),
            &format!("Use {} lens", next.label()),
            &CameraAction::SetFacing { facing: next },
        ) {
            specs.push(if next == facing {
                spec.unavailable("already_selected", "This lens is already selected")
            } else {
                spec
            });
        }
    }
    specs
}

/// Capture-settings Document child for [`tilo_resource::publish`].
pub(crate) fn settings_document_resource(settings: &Settings) -> Resource {
    let mut node = Resource::new("camera.settings", "camera.settings", "Capture settings");
    for resolution in Resolution::ALL {
        if let Some(spec) = immediate_spec(
            &format!("resolution-{}", resolution.as_str()),
            &format!("Use {} resolution", resolution.label()),
            &CameraAction::SetResolution { resolution },
        ) {
            node = node.with_action(if resolution == settings.resolution {
                spec.unavailable("already_selected", "This resolution is already selected")
            } else {
                spec
            });
        }
    }
    for photo_format in PhotoFormat::ALL {
        if let Some(spec) = immediate_spec(
            &format!("format-{}", photo_format.as_str()),
            &format!("Use {} photos", photo_format.label()),
            &CameraAction::SetPhotoFormat { photo_format },
        ) {
            node = node.with_action(if photo_format == settings.photo_format {
                spec.unavailable("already_selected", "This photo format is already selected")
            } else {
                spec
            });
        }
    }
    for timer in Timer::ALL {
        if let Some(spec) = immediate_spec(
            &format!("timer-{}", timer.seconds()),
            &format!("Use {} timer", timer.label()),
            &CameraAction::SetTimer { timer },
        ) {
            node = node.with_action(if timer == settings.timer {
                spec.unavailable("already_selected", "This timer is already selected")
            } else {
                spec
            });
        }
    }
    for (id, label, action) in [
        (
            "grid",
            if settings.grid {
                "Turn off composition grid"
            } else {
                "Turn on composition grid"
            },
            CameraAction::SetGrid { on: !settings.grid },
        ),
        (
            "level",
            if settings.level {
                "Turn off level guide"
            } else {
                "Turn on level guide"
            },
            CameraAction::SetLevel {
                on: !settings.level,
            },
        ),
        (
            "mirror-front",
            if settings.mirror_front {
                "Turn off front-camera mirroring"
            } else {
                "Turn on front-camera mirroring"
            },
            CameraAction::SetMirrorFront {
                on: !settings.mirror_front,
            },
        ),
        (
            "geotag",
            if settings.geotag {
                "Turn off photo location"
            } else {
                "Turn on photo location"
            },
            CameraAction::SetGeotag {
                on: !settings.geotag,
            },
        ),
        (
            "hdr",
            if settings.hdr {
                "Turn off HDR"
            } else {
                "Turn on HDR"
            },
            CameraAction::SetHdr { on: !settings.hdr },
        ),
    ] {
        if let Some(spec) = immediate_spec(id, label, &action) {
            node = node.with_action(spec);
        }
    }
    for next in Facing::ALL {
        if let Some(spec) = immediate_spec(
            &format!("default-lens-{}", next.as_str()),
            &format!("Use {} as the default camera", next.label()),
            &CameraAction::SetDefaultFacing { facing: next },
        ) {
            node = node.with_action(if next == settings.default_facing {
                spec.unavailable(
                    "already_selected",
                    "This default camera is already selected",
                )
            } else {
                spec
            });
        }
    }
    node
}

/// Document-derived children: settings, recent handoff, scan history rows.
pub(crate) fn publish_document_children(
    settings: &Settings,
    recent: Option<&CaptureItem>,
    scans: &[crate::model::ScanEntry],
) -> Vec<Resource> {
    let mut children = vec![settings_document_resource(settings)];
    if let Some(it) = recent {
        children.push(capture_resource(it));
    }
    for scan in scans.iter().take(32) {
        children.push(Resource::new(
            format!("camera.scan:{}", scan.ts),
            "camera.scan",
            scan.raw.clone(),
        ));
    }
    children
}

fn capture_effect_root(ctx: &CameraResourceCtx) -> Resource {
    let mut root = Resource::new("camera.capture", "camera.capture", "Capture");
    root = root.with_action(open_photos_action());
    if let Ok(spec) = ActionSpec::operation(
        "capture",
        "Capture photo",
        OperationKind::Effect,
        CAMERA_OPERATION_TYPE,
        &CameraOperation::Capture,
    ) {
        root = root.with_action(if !ctx.feed_available {
            spec.unavailable(
                "camera_unavailable",
                "Connect or allow a camera before capturing",
            )
        } else if ctx.mode == CaptureMode::Photo && !ctx.recording {
            spec
        } else {
            spec.unavailable(
                "photo_mode_required",
                "Switch to photo mode before capturing",
            )
        });
    }
    if !ctx.feed_available {
        if let Ok(spec) = ActionSpec::operation(
            "retry-camera",
            "Refresh cameras",
            OperationKind::Effect,
            CAMERA_OPERATION_TYPE,
            &CameraOperation::RetryCamera,
        ) {
            root = root.with_action(spec);
        }
        if ctx.camera_permission_denied
            && let Ok(spec) = ActionSpec::operation(
                "open-camera-privacy",
                "Open camera privacy settings",
                OperationKind::Effect,
                CAMERA_OPERATION_TYPE,
                &CameraOperation::OpenCameraPrivacySettings,
            )
        {
            root = root.with_action(spec);
        }
        if ctx.camera_devices.len() > 1 {
            for device in ctx.camera_devices.iter().take(MAX_PUBLISHED_CAMERA_DEVICES) {
                let label = if ctx.selected_camera.as_deref() == Some(device.as_str()) {
                    format!("Use {device} (selected)")
                } else {
                    format!("Use {device}")
                };
                if let Ok(spec) = ActionSpec::operation(
                    format!("camera-device-{device}"),
                    label,
                    OperationKind::Effect,
                    CAMERA_OPERATION_TYPE,
                    &CameraOperation::SelectCamera {
                        device: device.clone(),
                    },
                ) {
                    root = root.with_action(
                        if ctx.selected_camera.as_deref() == Some(device.as_str()) {
                            spec.unavailable("already_selected", "This camera is already selected")
                        } else {
                            spec
                        },
                    );
                }
            }
        }
    }
    if let Ok(spec) = ActionSpec::operation(
        "record",
        if ctx.recording {
            "Stop recording"
        } else {
            "Start recording"
        },
        OperationKind::Effect,
        CAMERA_OPERATION_TYPE,
        &CameraOperation::ToggleRecording,
    ) {
        root = root.with_action(if !ctx.feed_available {
            spec.unavailable(
                "camera_unavailable",
                "Connect or allow a camera before recording",
            )
        } else if ctx.mode == CaptureMode::Video {
            spec
        } else {
            spec.unavailable(
                "video_mode_required",
                "Switch to video mode before recording",
            )
        });
    }
    root
}

fn scanner_effect_root(ctx: &CameraResourceCtx) -> Resource {
    let mut scanner = Resource::new("camera.scanner", "camera.scanner", "Code scanner");
    if let Ok(spec) = ActionSpec::operation(
        "import-image",
        "Scan existing image",
        OperationKind::Effect,
        CAMERA_OPERATION_TYPE,
        &CameraOperation::ImportCodeImage,
    ) {
        scanner = scanner.with_action(if ctx.scan_import_pending {
            spec.unavailable("picker_open", "An image picker is already open")
        } else {
            spec
        });
    }
    scanner
}

fn document_effect_root(ctx: &CameraResourceCtx) -> Resource {
    let mut document = Resource::new(
        "camera.document-scanner",
        "camera.document-scanner",
        "Document scanner",
    );
    let document_actions = [
        (
            "capture-page",
            "Capture document page",
            CameraOperation::CaptureDocumentPage,
            ctx.document_scanner_available,
        ),
        (
            "rotate-page",
            "Rotate selected page",
            CameraOperation::RotateDocumentPage,
            ctx.document_pages > 0,
        ),
        (
            "recognize-text",
            "Recognize text on selected page",
            CameraOperation::RecognizeDocumentPage,
            ctx.document_pages > 0,
        ),
        (
            "copy-text",
            "Copy recognized text",
            CameraOperation::CopyDocumentText,
            ctx.document_has_text,
        ),
        (
            "export-pdf",
            "Export searchable PDF",
            CameraOperation::ExportDocument,
            ctx.document_pages > 0,
        ),
        (
            "crop-page",
            "Crop selected page",
            CameraOperation::BeginDocumentCrop,
            ctx.document_pages > 0,
        ),
        (
            "move-page-earlier",
            "Move selected page earlier",
            CameraOperation::MoveDocumentPageEarlier,
            ctx.document_pages > 0 && ctx.document_selected > 0,
        ),
        (
            "move-page-later",
            "Move selected page later",
            CameraOperation::MoveDocumentPageLater,
            ctx.document_pages > 0 && ctx.document_selected + 1 < ctx.document_pages,
        ),
        (
            "remove-page",
            "Remove selected page",
            CameraOperation::RemoveDocumentPage,
            ctx.document_pages > 0,
        ),
    ];
    for (id, label, operation, available) in document_actions {
        let kind = if operation == CameraOperation::BeginDocumentCrop {
            OperationKind::View
        } else {
            OperationKind::Effect
        };
        if let Ok(spec) = ActionSpec::operation(id, label, kind, CAMERA_OPERATION_TYPE, &operation)
        {
            document = document.with_action(if ctx.document_busy {
                spec.unavailable("document_busy", "Wait for the current document task")
            } else if available {
                spec
            } else if operation == CameraOperation::CaptureDocumentPage {
                spec.unavailable("no_scanner", "Connect or select a document scanner first")
            } else {
                spec.unavailable("no_selected_page", "Capture and select a page first")
            });
        }
    }
    document
}

/// Hosted-only extras that are not document state: shutter / record / device
/// chooser, code-scanner import, and document-scanner Effects. Mode, lens and
/// settings Document ops live on [`tilo_resource::publish`].
pub(crate) fn live_effect_extras(ctx: &CameraResourceCtx) -> Vec<Resource> {
    vec![
        capture_effect_root(ctx),
        scanner_effect_root(ctx),
        document_effect_root(ctx),
    ]
}

fn state_from_ctx(ctx: &CameraResourceCtx) -> crate::resource::CameraState {
    crate::resource::CameraState {
        mode: ctx.mode,
        facing: ctx.facing,
        settings: ctx.settings,
        scans: Vec::new(),
        recent: ctx.recent.as_ref().map(|item| doc::SharedCapture {
            id: item.id.clone(),
            media_id: item.media_id.clone().unwrap_or_default(),
            kind: if item.is_video {
                CaptureKind::Video
            } else {
                CaptureKind::Photo
            },
            ts: 0,
            w: 0,
            h: 0,
            dur_ms: 0,
            label: item.label.clone(),
            facing: ctx.facing,
        }),
    }
}

/// The published camera door plus live shutter and scanner extras.
///
/// Document mode, lens, settings, and the recent handoff come from
/// [`tilo_resource::publish`]. Capture, code-scanner, and document-scanner
/// Effects stay beside that root because they are device I/O, not shared state.
pub fn provide_resources(ctx: &CameraResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    // Mode, lens, and settings are the document once `mode` has been written.
    // A caller that has not written them yet still publishes its context.
    // Shutter and scanner Effects stay live extras.
    let has_document = automerge::ReadDoc::get(doc, &automerge::ROOT, "mode")
        .ok()
        .flatten()
        .is_some();
    let state = if has_document {
        <crate::resource::Camera as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_else(|_| state_from_ctx(ctx))
    } else {
        state_from_ctx(ctx)
    };
    let mut resources = vec![ui::tilo_resource::publish::<crate::resource::Camera>(
        crate::resource::CAMERA_RESOURCE_ID,
        "Camera",
        &state,
    )];
    resources.extend(live_effect_extras(ctx));
    resources
}

fn find_resource(resource: &Resource, id: &str) -> Option<Resource> {
    if resource.id.as_str() == id {
        return Some(resource.clone());
    }
    resource
        .children
        .iter()
        .find_map(|child| find_resource(child, id))
}

/// Resolve a stable root or the single recent-capture handoff from the
/// published camera tree. Camera never enumerates a roll.
pub fn resolve_resource(ctx: &CameraResourceCtx, doc: &AutoCommit, id: &str) -> Option<Resource> {
    provide_resources(ctx, doc)
        .iter()
        .find_map(|resource| find_resource(resource, id))
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::CameraApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let state =
            <crate::resource::Camera as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
        let mut resources = vec![ui::tilo_resource::publish::<crate::resource::Camera>(
            crate::resource::CAMERA_RESOURCE_ID,
            "Camera",
            &state,
        )];
        resources.extend(live_effect_extras(&self.camera_resource_ctx()));
        resources
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.resolve_camera_resource(doc, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CameraApp;
    use tilo_actions::ActionInput;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// A hermetic Camera app with its sample captures synced into a fresh
    /// document. The sample set exposes one contextual recent handoff.
    fn seeded() -> (CameraApp, AutoCommit) {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// The resource tree a sample Camera publishes once it has synced `doc`.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let mut app = CameraApp::sample();
        app.sync(&mut doc.clone(), &ROOT);
        ui::resources::resources(&app, doc)
    }

    fn encoded(action: &CameraAction) -> EncodedAction {
        encode(action).unwrap()
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| published(doc),
        palette: [
            "Use video mode",
            "Use 4K resolution",
            "Turn off composition grid",
            "Open Photos",
        ],
        replay: [
            // The scan-history row-insert: dedup + cap run deterministically
            // against the shared base on each replica.
            record_scan => |_| encoded(&record_scan_action("https://example.test/hello", 1234)),
            set_grid => |doc| encoded(&CameraAction::SetGrid {
                on: !doc::read_settings(doc, &ROOT).grid,
            }),
            register_capture => |_| encoded(&CameraAction::RegisterCapture {
                id: "IMG_peer.jpg".into(),
                media_id: crate::media::store(
                    &tilo_content::ContentStore::memory(),
                    b"portable camera bytes",
                ).unwrap(),
                kind: crate::model::CaptureKind::Photo,
                ts: 42,
                w: 64,
                h: 48,
                dur_ms: 0,
                label: "Peer photo".into(),
                facing: Facing::Back,
            }),
        ],
        undo: [
            record_scan => |_| encoded(&record_scan_action("https://example.test/undo", 99)),
            set_mode => |_| encoded(&CameraAction::SetMode { mode: CaptureMode::Video }),
        ],
        encodes: [
            video_mode => ("camera", "video-mode", |_, _| {
                encoded(&CameraAction::SetMode { mode: CaptureMode::Video })
            }),
            resolution_4k => ("camera.settings", "resolution-4k", |_, _| {
                encoded(&CameraAction::SetResolution { resolution: Resolution::R4K })
            }),
            grid => ("camera.settings", "grid", |doc, _| {
                encoded(&CameraAction::SetGrid {
                    on: !doc::read_settings(doc, &ROOT).grid,
                })
            }),
        ],
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn published_resources_hand_captures_to_their_owners_and_refuse_library_management() {
        // Phase `published_resources_respect_camera_ownership`.
        // Camera publishes capture/scanner/settings operations plus at most one
        // contextual handoff, never a media library management surface.
        {
            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            assert!(
                resources.len() >= 3,
                "capture, scanner, document scanner, settings, and Camera discovery"
            );
            let captures = resources
                .iter()
                .find(|resource| resource.id.as_str() == "camera.capture")
                .expect("capture root");
            assert_eq!(captures.kind, "camera.capture");
            let browse = captures
                .actions
                .iter()
                .find(|action| action.id == "open-photos")
                .expect("capture surface routes browsing to Photos");
            assert_eq!(
                browse.operation_type.as_deref(),
                Some(APPLICATION_LAUNCH_TYPE)
            );
            let ActionInput::Immediate(encoded) = &browse.input else {
                panic!("Photos browse is a zero-argument application handoff");
            };
            let browse_payload: serde_json::Value =
                serde_json::from_slice(&encoded.payload).expect("Photos handoff payload JSON");
            assert_eq!(browse_payload["app_id"].as_str(), Some("so.tilo.photos"));
            assert!(browse_payload.get("content_hash").is_none());
            assert!(captures.children.len() <= 1);
            let camera = resources
                .iter()
                .find(|resource| resource.id.as_str() == "camera")
                .expect("camera resource");
            for resource in camera.children.iter().filter(|resource| {
                matches!(resource.kind.as_str(), "camera.photo" | "camera.video")
            }) {
                let action = resource
                    .actions
                    .iter()
                    .find(|action| action.id == "open")
                    .expect("capture has its typed open handoff");
                assert_eq!(
                    action.operation_type.as_deref(),
                    Some(APPLICATION_LAUNCH_TYPE)
                );
                assert!(
                    !resource.actions.iter().any(|action| {
                        matches!(action.id.as_str(), "favorite" | "share" | "delete-file")
                    }),
                    "Camera does not publish media-library management"
                );
                let ActionInput::Immediate(encoded) = &action.input else {
                    panic!("zero-argument handoff must be immediate");
                };
                let payload: serde_json::Value =
                    serde_json::from_slice(&encoded.payload).expect("handoff payload JSON");
                assert!(matches!(
                    payload["app_id"].as_str(),
                    Some("so.tilo.photos" | "so.tilo.video-player")
                ));
                assert!(
                    payload["content_hash"]
                        .as_str()
                        .is_some_and(|hash| hash.len() == 64)
                );
            }
            let ids: Vec<_> = resources
                .iter()
                .flat_map(|resource| {
                    std::iter::once(resource.id.as_str())
                        .chain(resource.children.iter().map(|child| child.id.as_str()))
                })
                .collect();
            assert!(ids.contains(&"camera.scanner"));
            assert!(ids.contains(&"camera.document-scanner"));
            assert!(ids.contains(&"camera.settings"));
            assert!(
                !ids.iter()
                    .any(|id| { matches!(*id, "so.tilo.photos.app" | "so.tilo.video-player.app") }),
                "handoff applications are destinations on the current capture, never Camera roots"
            );
            assert!(
                !ids.iter()
                    .any(|id| id.contains("gallery") || id.contains("library"))
            );
        }
        // Phase `capture_handoff_destination_is_typed_and_kind_specific`.
        {
            assert_eq!(capture_destination(false), ("so.tilo.photos", "Photos"));
            assert_eq!(
                capture_destination(true),
                ("so.tilo.video-player", "Video Player")
            );
        }
        // Phase `capture_surface_exposes_at_most_one_recent_handoff`.
        {
            let ctx = CameraResourceCtx {
                recent: Some(CaptureItem {
                    id: "IMG_latest.jpg".into(),
                    label: "Latest capture".into(),
                    is_video: false,
                    media_id: Some(format!("{}{:064x}", crate::media::MEDIA_PREFIX, 1)),
                }),
                ..CameraResourceCtx::default()
            };
            let doc = AutoCommit::new();
            let page = provide_resources(&ctx, &doc);
            let handoffs: Vec<_> = page[0]
                .children
                .iter()
                .filter(|child| matches!(child.kind.as_str(), "camera.photo" | "camera.video"))
                .collect();
            assert_eq!(handoffs.len(), 1);
            let recent = resolve_resource(&ctx, &doc, "camera.photo:IMG_latest.jpg")
                .expect("recent capture resolves");
            let open = recent
                .actions
                .iter()
                .find(|action| action.id == "open")
                .expect("capture has a canonical open action");
            assert_eq!(open.kind, OperationKind::Effect);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn provided_resources_are_honest_about_devices_and_expose_every_preference() {
        // Phase `camera_root_is_a_local_reveal_operation_and_discoverable_resource`.
        {
            let resources = provide_resources(&CameraResourceCtx::default(), &AutoCommit::new());
            let camera = resources
                .iter()
                .find(|resource| resource.id.as_str() == "camera")
                .expect("camera discovery resource");
            assert!(camera.actions.iter().any(|spec| spec.id == "open-scanner"));
            let scanner = camera
                .actions
                .iter()
                .find(|spec| spec.id == "open-scanner")
                .expect("scanner reveal action");
            assert_eq!(scanner.kind, OperationKind::View);
        }
        // Phase `ctrl_k_exposes_every_capture_preference`.
        {
            let resources = provide_resources(&CameraResourceCtx::default(), &AutoCommit::new());
            let settings = resources
                .iter()
                .flat_map(|resource| std::iter::once(resource).chain(resource.children.iter()))
                .find(|resource| resource.id.as_str() == "camera.settings")
                .expect("stable settings resource");
            let ids: Vec<&str> = settings
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            for id in [
                "resolution-4k",
                "format-png",
                "timer-3",
                "grid",
                "level",
                "mirror-front",
                "geotag",
                "hdr",
                "default-lens-front",
            ] {
                assert!(ids.contains(&id), "missing settings action {id}: {ids:?}");
            }
        }
        // Phase `device_effects_are_honest_and_unverified_captures_publish_no_action`.
        {
            let resources = provide_resources(
                &CameraResourceCtx {
                    recent: Some(CaptureItem {
                        id: "IMG_1.jpg".into(),
                        label: "Capture".into(),
                        is_video: false,
                        media_id: None,
                    }),
                    ..CameraResourceCtx::default()
                },
                &AutoCommit::new(),
            );
            let capture_root = resources
                .iter()
                .find(|resource| resource.id.as_str() == "camera.capture")
                .expect("capture effect root");
            let capture = capture_root
                .actions
                .iter()
                .find(|a| a.id == "capture")
                .unwrap();
            assert_eq!(capture.kind, OperationKind::Effect);
            assert!(
                !capture.contract.availability.is_available(),
                "a palette must not claim capture works without a feed"
            );
            let unverified = resources[0]
                .children
                .iter()
                .find(|child| child.kind.as_str() == "camera.photo")
                .expect("recent handoff");
            assert!(unverified.actions.is_empty());

            let available = provide_resources(
                &CameraResourceCtx {
                    feed_available: true,
                    ..CameraResourceCtx::default()
                },
                &AutoCommit::new(),
            );
            let capture = available
                .iter()
                .find(|resource| resource.id.as_str() == "camera.capture")
                .expect("capture effect root")
                .actions
                .iter()
                .find(|action| action.id == "capture")
                .unwrap();
            assert!(capture.contract.availability.is_available());
        }
        // Phase `unavailable_camera_publishes_real_recovery_operations`.
        {
            let resources = provide_resources(
                &CameraResourceCtx {
                    camera_permission_denied: true,
                    camera_devices: vec!["/dev/video0".into(), "/dev/video2".into()],
                    ..CameraResourceCtx::default()
                },
                &AutoCommit::new(),
            );
            let actions = &resources
                .iter()
                .find(|resource| resource.id.as_str() == "camera.capture")
                .expect("capture effect root")
                .actions;
            let ids: Vec<&str> = actions.iter().map(|action| action.id.as_str()).collect();
            for id in [
                "retry-camera",
                "open-camera-privacy",
                "camera-device-/dev/video0",
                "camera-device-/dev/video2",
            ] {
                assert!(ids.contains(&id), "missing recovery action {id}: {ids:?}");
            }
            assert!(
                actions
                    .iter()
                    .filter(|action| {
                        matches!(
                            action.id.as_str(),
                            "retry-camera"
                                | "open-camera-privacy"
                                | "camera-device-/dev/video0"
                                | "camera-device-/dev/video2"
                        )
                    })
                    .all(|action| action.kind == OperationKind::Effect)
            );
        }
    }
}
