//! What Camera offers beyond its record's verbs: its effects, and the live
//! rows the surface publishes.
//!
//! The camera's *pixels* are verified content-store bytes with an optional
//! local file projection, and the *device I/O* around them (opening a lens,
//! decoding a QR out of a frame, the live viewfinder) is not collaborative.
//! The record ([`crate::record::Record`]) holds the portable metadata: the
//! recent capture handoff and the scan history.
//!
//! Capturing a photo or video, scanning codes and documents, switching
//! screens, mode and lens, and changing the capture preferences (settings of
//! the person and of this device, never record state) are the
//! [`CameraEffect`] vocabulary. Media-library management is owned by Photos
//! or Video Player. Zoom and an open chooser are presentation-only.

use ui::framework::LiveRow;
use ui::tilo_actions::ActionSpec;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource::{self, Effect};

use crate::model::{CaptureKind, CaptureMode, Facing, Settings};
use crate::record::SharedCapture;

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
#[effect(operation = "camera.effect")]
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
    #[effect(attended, row, job)]
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
    #[effect(attended, row, job)]
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

/// The recent capture, offering to open its verified bytes in Photos or
/// Video Player.
pub(crate) fn recent_row(it: &CaptureItem) -> LiveRow {
    let kind = if it.is_video {
        "camera.video"
    } else {
        "camera.photo"
    };
    let mut row = LiveRow::new(capture_resource_id(it), kind, item_label(it));
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
        row = row.operation(spec);
    }
    row
}

pub(crate) fn capture_item_from_shared(shared: &SharedCapture) -> CaptureItem {
    CaptureItem {
        id: shared.id.clone(),
        label: shared.label.clone(),
        is_video: matches!(shared.kind, CaptureKind::Video),
        media_id: Some(shared.media_id.clone()).filter(|id| !id.is_empty()),
    }
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
                spec.type_mut().label = format!("Use {device}");
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
        // Finalizing an existing clip needs no live camera. Keep the semantic
        // stop door available after disconnection, just like the shutter.
        if ctx.recording {
            None
        } else if !ctx.feed_available {
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
        record.type_mut().label = "Stop recording".into();
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

/// What the surface publishes beside the record's verbs: the capture
/// pipeline with its cameras, the code scanner and the document scanner,
/// each with the effects it offers now, and the recent capture with its
/// handoff to the app that owns it. Mode, lens and the capture preferences
/// are effects on the resource.
pub(crate) fn live_rows(ctx: &CameraResourceCtx) -> Vec<LiveRow> {
    let recent = ctx.recent.as_ref().map(recent_row);
    [capture_row(ctx), scanner_row(ctx), document_row(ctx)]
        .into_iter()
        .chain(recent)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CameraApp;
    use crate::record::Record;
    use ui::tilo_actions::{OperationKind, Resource};

    /// A record holding the sample recent capture.
    fn seeded() -> Record {
        let mut record = ui::tilo_resource::Replica::<Record>::new();
        record
            .apply(Record::REGISTER_CAPTURE.with(crate::resource::sample_capture()))
            .unwrap();
        record.state().unwrap()
    }

    /// The resource `state` publishes, with the live rows of `ctx` beside it.
    fn tree(ctx: &CameraResourceCtx, state: &Record) -> Vec<Resource> {
        let mut resources = vec![ui::tilo_resource::publish::<Record>(
            "camera", "Camera", state,
        )];
        resources.extend(live_rows(ctx).into_iter().map(LiveRow::into_resource));
        resources
    }

    /// The tree for `ctx` alone: its rows and recent capture.
    fn provide_resources(ctx: &CameraResourceCtx) -> Vec<Resource> {
        tree(ctx, &Record::default())
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

    /// What a sample Camera publishes once it shows `state`.
    fn published(state: &Record) -> Vec<Resource> {
        let mut app = CameraApp::sample();
        app.present_record(state);
        tree(&app.camera_resource_ctx(), state)
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
            let resources = published(&seeded());
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
            let Some(encoded) = browse.encode_immediate() else {
                panic!("Photos browse is a zero-argument application handoff");
            };
            let browse_payload: serde_json::Value =
                serde_json::from_slice(&encoded.payload).expect("Photos handoff payload JSON");
            assert_eq!(browse_payload["app_id"].as_str(), Some("so.tilo.photos"));
            assert!(browse_payload.get("content_hash").is_none());
            assert!(captures.children.len() <= 1);
            assert!(
                resources
                    .iter()
                    .any(|resource| resource.kind.as_str() == "camera.photo"),
                "the recent capture is offered"
            );
            for resource in resources.iter().filter(|resource| {
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
                let Some(encoded) = action.encode_immediate() else {
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
        {
            assert_eq!(capture_destination(false), ("so.tilo.photos", "Photos"));
            assert_eq!(
                capture_destination(true),
                ("so.tilo.video-player", "Video Player")
            );
        }
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
                page.iter()
                    .filter(|row| matches!(row.kind.as_str(), "camera.photo" | "camera.video"))
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
    fn disconnected_camera_keeps_stop_recording_available() {
        let resources = provide_resources(&CameraResourceCtx {
            mode: CaptureMode::Video,
            recording: true,
            feed_available: false,
            ..CameraResourceCtx::default()
        });
        let capture = resources
            .iter()
            .find(|resource| resource.id.as_str() == "camera.capture")
            .expect("capture effect root");
        let record = capture
            .actions
            .iter()
            .find(|action| action.id == "record")
            .expect("recording action");
        assert_eq!(record.label, "Stop recording");
        assert!(record.contract.availability.is_available());
        let photo = capture
            .actions
            .iter()
            .find(|action| action.id == "capture")
            .expect("photo action");
        assert!(!photo.contract.availability.is_available());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn provided_resources_are_honest_about_devices_and_expose_every_preference() {
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
        {
            let ids: Vec<String> = CameraEffect::specs()
                .into_iter()
                .map(|spec| spec.id.clone())
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
            let unverified = resources
                .iter()
                .find(|row| row.kind.as_str() == "camera.photo")
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
