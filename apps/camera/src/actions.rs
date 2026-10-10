//! Camera's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! The camera's *pixels* are verified content-store bytes with an optional local file
//! projection — a shutter press writes a PNG/JPEG, a recording writes a clip
//! — and the *device I/O* around them
//! (opening a lens, decoding a QR out of a frame, the live viewfinder) is
//! external, not collaborative. What lives in the shared Automerge document
//! (see [`crate::doc`]) is the *metadata*: portable capture registrations
//! and scan history. Those durable records are [`CameraAction`]s: small serializable values that
//! deterministically mutate the CRDT and therefore replicate, replay and undo
//! for free.
//!
//! ## What is *not* an action
//!
//! Capturing a photo/video (the frame write is device I/O), scanning codes
//! and documents, switching screens, mode and lens, and changing the capture
//! preferences (settings of the person and of this device, never document
//! state) are the [`CameraEffect`] vocabulary. Media-library management is owned by Photos or Video Player.
//! The one recent handoff, zoom, and an open chooser are presentation-only.
//!
//! ## Determinism contract
//!
//! Variants carry everything [`Action::apply`] needs (the item id, the scanned
//! payload, and a timestamp minted at authoring time); `apply` never touches
//! the RNG or the wall-clock. A malformed payload is an explicit
//! [`ActionError::Refused`], so the invoker never mistakes a rejected durable
//! mutation for success.

use automerge::ROOT;
use serde::{Deserialize, Serialize};

use tilo_actions::{Action, ActionError, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource};
use ui::framework::LiveRow;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource::{self, Effect};

use crate::doc;
use crate::model::{CaptureKind, CaptureMode, Facing, Settings};

/// Registry `type_id` for every Camera action.
pub const CAMERA_ACTION_TYPE: &str = "camera.action";
pub use ui::tilo_actions::remote::ApplicationLaunch;
/// Device discovery is external and potentially hostile. Publishing a small,
/// deterministic prefix keeps resource/search materialization bounded.
pub const MAX_PUBLISHED_CAMERA_DEVICES: usize = 16;

/// What Camera does on this device and what it shows, which change no
/// document: the screens, the shutter and recorder, the cameras it can open,
/// scanning a code out of an existing image, and the document scanner's
/// session. Everything reaches this device's cameras, scanners, files or
/// clipboard, so only the person at it may invoke them. Work on the capture
/// pipeline, the code scanner and the document scanner rides the row of the
/// same name; choosing a camera rides that camera's row.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "camera.effect", device_only)]
pub enum CameraEffect {
    /// Open Camera
    #[effect(view)]
    OpenCamera,
    /// Open Scan
    #[effect(view)]
    OpenScanner,
    /// Open Documents
    #[effect(view)]
    OpenDocument,
    /// Open Settings
    #[effect(view)]
    OpenSettings,
    /// Use capture mode
    ///
    /// Switches the viewfinder between photo and video. Unavailable while a
    /// video records.
    #[effect(view)]
    UseMode {
        #[param("Capture mode", choices = ["photo", "video"])]
        mode: String,
    },
    /// Use lens
    #[effect(view)]
    UseLens {
        #[param("Lens", choices = ["back", "front", "wide"])]
        lens: String,
    },
    /// Set resolution
    ///
    /// The resolution this device's cameras capture at.
    SetResolution {
        #[param("Resolution", choices = ["12mp", "4k", "1080p", "720p"])]
        resolution: String,
    },
    /// Set photo format
    SetPhotoFormat {
        #[param("Photo format", choices = ["jpeg", "png"])]
        format: String,
    },
    /// Set self-timer
    SetTimer {
        #[param("Seconds before the shutter", choices = ["0", "3", "10"])]
        seconds: String,
    },
    /// Show composition grid
    SetGrid {
        #[param("Whether the grid is shown")]
        on: bool,
    },
    /// Show level guide
    SetLevel {
        #[param("Whether the level guide is shown")]
        on: bool,
    },
    /// Mirror front camera
    SetMirrorFront {
        #[param("Whether front-camera photos are mirrored")]
        on: bool,
    },
    /// Tag photo location
    SetGeotag {
        #[param("Whether photos record where they were taken")]
        on: bool,
    },
    /// Use HDR
    SetHdr {
        #[param("Whether photos use HDR")]
        on: bool,
    },
    /// Set default lens
    ///
    /// The lens this device's Camera opens with.
    SetDefaultLens {
        #[param("Lens", choices = ["back", "front", "wide"])]
        lens: String,
    },
    /// Capture photo
    ///
    /// Saves the current frame and answers once the photo is registered.
    #[effect(row, job, cancellable)]
    Capture,
    /// Start recording
    ///
    /// Starts a clip, or stops the one recording and answers once it is
    /// saved and registered.
    #[effect(row, job, cancellable)]
    Record,
    /// Refresh cameras
    ///
    /// Drops a stale camera session and looks for cameras again.
    #[effect(row, idempotent)]
    RetryCamera,
    /// Open camera privacy settings
    #[effect(row)]
    OpenCameraPrivacy,
    /// Use camera
    #[effect(row)]
    UseCamera {
        #[param("The camera's device node, as discovered on this device")]
        device: String,
    },
    /// Scan existing image
    ///
    /// Asks for an image with the file chooser and decodes the QR code or
    /// barcode in it, without importing the image.
    #[effect(row, job)]
    ImportImage,
    /// Capture document page
    #[effect(row, job, cancellable)]
    CapturePage,
    /// Rotate selected page
    #[effect(row)]
    RotatePage,
    /// Recognize text on selected page
    #[effect(row, job, cancellable)]
    RecognizeText,
    /// Copy recognized text
    #[effect(row)]
    CopyText,
    /// Export searchable PDF
    ///
    /// Asks where to save the scanned pages as one searchable PDF.
    #[effect(row, job)]
    ExportPdf,
    /// Crop selected page
    #[effect(view, row)]
    CropPage,
    /// Move selected page earlier
    #[effect(row)]
    MovePageEarlier,
    /// Move selected page later
    #[effect(row)]
    MovePageLater,
    /// Remove selected page
    #[effect(row)]
    RemovePage,
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
    /// Record a decoded code into the shared scan history. The QR/barcode
    /// decode is device I/O (external); this metadata row-insert is the
    /// replayable action. `ts` is minted at authoring time and carried here so
    /// `apply` stays pure; the insert deduplicates and caps deterministically
    /// against the replay base.
    RecordScan { raw: String, ts: i64 },
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

/// The compact presentation context Camera builds its live rows from.
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
    ApplicationLaunch::app("so.tilo.photos").spec(
        "open-photos",
        "Open Photos",
        "Browse your photo library in Photos",
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
        let spec = ApplicationLaunch::with_content(app_id, content_hash).spec(
            "open",
            &label,
            "Open the verified portable capture in its owning app",
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

/// Document-derived children: the recent handoff and scan history rows.
pub(crate) fn publish_document_children(
    recent: Option<&CaptureItem>,
    scans: &[crate::model::ScanEntry],
) -> Vec<Resource> {
    let mut children = Vec::new();
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

/// The row of the capture pipeline published by the surface.
pub(crate) const CAPTURE_ROW: &str = "camera.capture";
/// The row of the code scanner published by the surface.
pub(crate) const SCANNER_ROW: &str = "camera.scanner";
/// The row of the document scanner published by the surface.
pub(crate) const DOCUMENT_ROW: &str = "camera.document-scanner";

/// The published row of the camera at `device`.
pub(crate) fn camera_device_row_id(device: &str) -> String {
    format!("camera.device:{device}")
}

/// `effect` bound to its row, or unavailable for `why` (a code for
/// programs, a reason for people).
fn bound(effect: &CameraEffect, why: Option<(&str, &str)>) -> ActionSpec {
    let spec = effect.bound();
    match why {
        Some((code, reason)) => spec.unavailable(code, reason),
        None => spec,
    }
}

fn capture_row(ctx: &CameraResourceCtx) -> LiveRow {
    let mut row = LiveRow::new(CAPTURE_ROW, "camera.capture", "Capture")
        .operation(open_photos_action())
        .operation(bound(
            &CameraEffect::Capture,
            if !ctx.feed_available {
                Some((
                    "camera_unavailable",
                    "Connect or allow a camera before capturing",
                ))
            } else if ctx.mode == CaptureMode::Photo && !ctx.recording {
                None
            } else {
                Some((
                    "photo_mode_required",
                    "Switch to photo mode before capturing",
                ))
            },
        ));
    if !ctx.feed_available {
        row = row.effect(&CameraEffect::RetryCamera);
        if ctx.camera_permission_denied {
            row = row.effect(&CameraEffect::OpenCameraPrivacy);
        }
        if ctx.camera_devices.len() > 1 {
            for device in ctx.camera_devices.iter().take(MAX_PUBLISHED_CAMERA_DEVICES) {
                let selected = ctx.selected_camera.as_deref() == Some(device.as_str());
                let mut spec = bound(
                    &CameraEffect::UseCamera {
                        device: device.clone(),
                    },
                    selected.then_some(("already_selected", "This camera is already selected")),
                );
                spec.label = format!("Use {device}");
                let label = if selected {
                    format!("{device} (selected)")
                } else {
                    device.clone()
                };
                row = row.row(
                    LiveRow::new(camera_device_row_id(device), "camera.device", label)
                        .operation(spec),
                );
            }
        }
    }
    let mut record = bound(
        &CameraEffect::Record,
        if !ctx.feed_available {
            Some((
                "camera_unavailable",
                "Connect or allow a camera before recording",
            ))
        } else if ctx.mode == CaptureMode::Video {
            None
        } else {
            Some((
                "video_mode_required",
                "Switch to video mode before recording",
            ))
        },
    );
    if ctx.recording {
        record.label = "Stop recording".into();
    }
    row.operation(record)
}

fn scanner_row(ctx: &CameraResourceCtx) -> LiveRow {
    LiveRow::new(SCANNER_ROW, "camera.scanner", "Code scanner").operation(bound(
        &CameraEffect::ImportImage,
        ctx.scan_import_pending
            .then_some(("picker_open", "An image picker is already open")),
    ))
}

fn document_row(ctx: &CameraResourceCtx) -> LiveRow {
    let has_page = ctx.document_pages > 0;
    let document_effects = [
        (CameraEffect::CapturePage, ctx.document_scanner_available),
        (CameraEffect::RotatePage, has_page),
        (CameraEffect::RecognizeText, has_page),
        (CameraEffect::CopyText, ctx.document_has_text),
        (CameraEffect::ExportPdf, has_page),
        (CameraEffect::CropPage, has_page),
        (
            CameraEffect::MovePageEarlier,
            has_page && ctx.document_selected > 0,
        ),
        (
            CameraEffect::MovePageLater,
            has_page && ctx.document_selected + 1 < ctx.document_pages,
        ),
        (CameraEffect::RemovePage, has_page),
    ];
    document_effects.into_iter().fold(
        LiveRow::new(DOCUMENT_ROW, "camera.document-scanner", "Document scanner"),
        |row, (effect, available)| {
            let why = if ctx.document_busy {
                Some(("document_busy", "Wait for the current document task"))
            } else if available {
                None
            } else if effect == CameraEffect::CapturePage {
                Some(("no_scanner", "Connect or select a document scanner first"))
            } else {
                Some(("no_selected_page", "Capture and select a page first"))
            };
            row.operation(bound(&effect, why))
        },
    )
}

/// What the surface publishes beside the document: the capture pipeline
/// with its cameras, the code scanner and the document scanner, each with
/// the effects it offers now. Mode, lens and the capture preferences are
/// effects on the resource.
pub(crate) fn live_rows(ctx: &CameraResourceCtx) -> Vec<LiveRow> {
    vec![capture_row(ctx), scanner_row(ctx), document_row(ctx)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CameraApp;
    use automerge::AutoCommit;
    use tilo_actions::ActionInput;
    use tilo_actions::ActionRegistry;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(<crate::resource::Camera as ui::tilo_resource::Resource>::register)
    }

    /// A hermetic Camera app synced with a record holding one recent capture.
    fn seeded() -> (CameraApp, AutoCommit) {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        crate::resource::sample_capture().apply(&mut doc).unwrap();
        app.sync(&doc, &ROOT);
        app.present_record(
            &<crate::resource::Camera as ui::tilo_resource::Resource>::read(&doc).unwrap(),
        );
        (app, doc)
    }

    /// The resource `state` publishes, with the live rows of `ctx` beside it.
    fn tree(ctx: &CameraResourceCtx, state: &crate::resource::CameraState) -> Vec<Resource> {
        let mut resources = vec![ui::tilo_resource::publish::<crate::resource::Camera>(
            crate::resource::CAMERA_RESOURCE_ID,
            "Camera",
            state,
        )];
        resources.extend(live_rows(ctx).into_iter().map(LiveRow::into_resource));
        resources
    }

    /// The tree for `ctx` alone: its rows and recent capture.
    fn provide_resources(ctx: &CameraResourceCtx) -> Vec<Resource> {
        let state = crate::resource::CameraState {
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
        };
        tree(ctx, &state)
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

    /// The resource a sample Camera publishes once it has synced `doc`.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let mut app = CameraApp::sample();
        app.sync(&doc.clone(), &ROOT);
        let state = <crate::resource::Camera as ui::tilo_resource::Resource>::read(doc).unwrap();
        app.present_record(&state);
        tree(&app.camera_resource_ctx(), &state)
    }

    fn encoded(action: &CameraAction) -> EncodedAction {
        encode(action).unwrap()
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| published(doc),
        palette: [
            "Open Photos",
        ],
        replay: [
            // The scan-history row-insert: dedup + cap run deterministically
            // against the shared base on each replica.
            record_scan => |_| encoded(&record_scan_action("https://example.test/hello", 1234)),
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
            let resources = published(&doc);
            let _ = app;
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
                Some(ApplicationLaunch::TYPE)
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
                    Some(ApplicationLaunch::TYPE)
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
            let page = provide_resources(&ctx);
            assert_eq!(
                page[0]
                    .children
                    .iter()
                    .filter(|child| matches!(child.kind.as_str(), "camera.photo" | "camera.video"))
                    .count(),
                1
            );
            let recent = page
                .iter()
                .find_map(|resource| find_resource(resource, "camera.photo:IMG_latest.jpg"))
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
        // Phase `screen_reveals_are_views_on_the_camera`.
        {
            let specs = CameraEffect::specs();
            let scanner = specs
                .iter()
                .find(|spec| spec.id == "open-scanner")
                .expect("scanner reveal effect");
            assert_eq!(scanner.kind, OperationKind::View);
            assert!(
                specs
                    .iter()
                    .filter(|spec| spec.kind == OperationKind::View)
                    .all(|spec| spec.id.starts_with("open-") || spec.id.starts_with("use-")),
                "only the screen, mode and lens reveals are views: {:?}",
                specs.iter().map(|spec| &spec.id).collect::<Vec<_>>()
            );
        }
        // Phase `ctrl_k_exposes_every_capture_preference`.
        {
            let ids: Vec<String> = CameraEffect::specs()
                .into_iter()
                .map(|spec| spec.id)
                .collect();
            for id in [
                "use-mode",
                "use-lens",
                "set-resolution",
                "set-photo-format",
                "set-timer",
                "set-grid",
                "set-level",
                "set-mirror-front",
                "set-geotag",
                "set-hdr",
                "set-default-lens",
            ] {
                assert!(
                    ids.iter().any(|known| known == id),
                    "missing preference {id}: {ids:?}"
                );
            }
        }
        // Phase `device_effects_are_honest_and_unverified_captures_publish_no_action`.
        {
            let resources = provide_resources(&CameraResourceCtx {
                recent: Some(CaptureItem {
                    id: "IMG_1.jpg".into(),
                    label: "Capture".into(),
                    is_video: false,
                    media_id: None,
                }),
                ..CameraResourceCtx::default()
            });
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

            let available = provide_resources(&CameraResourceCtx {
                feed_available: true,
                ..CameraResourceCtx::default()
            });
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
            let resources = provide_resources(&CameraResourceCtx {
                camera_permission_denied: true,
                camera_devices: vec!["/dev/video0".into(), "/dev/video2".into()],
                ..CameraResourceCtx::default()
            });
            let capture = resources
                .iter()
                .find(|resource| resource.id.as_str() == "camera.capture")
                .expect("capture effect root");
            let ids: Vec<&str> = capture
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            for id in ["retry-camera", "open-camera-privacy"] {
                assert!(ids.contains(&id), "missing recovery action {id}: {ids:?}");
            }
            for device in ["/dev/video0", "/dev/video2"] {
                let row = capture
                    .children
                    .iter()
                    .find(|row| row.id.as_str() == camera_device_row_id(device))
                    .unwrap_or_else(|| panic!("{device} is a row"));
                let choose = row
                    .actions
                    .iter()
                    .find(|action| action.id == "use-camera")
                    .expect("a camera row offers choosing it");
                assert_eq!(choose.label, format!("Use {device}"));
                assert!(choose.contract.availability.is_available());
            }
            assert!(
                capture
                    .actions
                    .iter()
                    .chain(capture.children.iter().flat_map(|row| row.actions.iter()))
                    .filter(|action| action.id != "open-photos")
                    .all(|action| action.kind == OperationKind::Effect
                        && action.operation_type.as_deref() == Some(CameraEffect::OPERATION))
            );
        }
    }
}
