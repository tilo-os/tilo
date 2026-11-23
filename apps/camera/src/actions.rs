//! What Camera offers beyond its resource's verbs: its effects, and the live
//! rows the surface publishes.
//!
//! The camera's *pixels* are verified content-store bytes with an optional
//! local file projection, and the *device I/O* around them (opening a lens,
//! decoding a QR out of a frame, the live viewfinder) is not collaborative.
//! The resource ([`crate::camera::Camera`]) holds the portable metadata: the
//! recent capture handoff and the scan history.
//!
//! Capturing a photo or video, scanning codes and documents, switching
//! screens, mode and lens, and changing the capture preferences (settings of
//! the person and of this device, never record state) are the
//! [`CameraEffect`] vocabulary. Media-library management is owned by Photos
//! or Video. Zoom and an open chooser are presentation-only.

use ui::framework::{EffectStatus, LiveRow};

use ui::tilo_resource::row::RowBindings;

use crate::row_types::{CameraDevice, Capture, DocumentScanner, Photo, Scanner, Video};
use ui::tilo_actions::ActionSpec;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource::{self, Effect};

use crate::model::{CaptureKind, CaptureMode, Facing, Settings};

pub use ui::tilo_actions::remote::ApplicationLaunch;
/// Device discovery is external and potentially hostile. Publishing a small,
/// deterministic prefix keeps resource/search materialization bounded.
pub const MAX_PUBLISHED_CAMERA_DEVICES: usize = 16;

/// The document scan settings [`CameraEffect::ChooseScanSetting`] chooses
/// among, in the order the document screen shows them.
pub const SCAN_SETTINGS: [&str; 5] = ["scanner", "resolution", "color", "source", "language"];

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
    /// video resources.
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
    #[effect(kind = "outside")]
    #[effect(label = "camera-settings-resolution")]
    SetResolution {
        #[param("Resolution", choices = ["12mp", "4k", "1080p", "720p"])]
        resolution: String,
    },
    /// Set photo format
    #[effect(kind = "outside")]
    #[effect(label = "camera-settings-photo-format")]
    SetPhotoFormat {
        #[param("Photo format", choices = ["jpeg", "png"])]
        format: String,
    },
    /// Set self-timer
    #[effect(kind = "outside")]
    #[effect(label = "camera-settings-timer")]
    SetTimer {
        #[param("Seconds before the shutter", choices = ["0", "3", "10"])]
        seconds: String,
    },
    /// Show composition grid
    #[effect(screen, label = "camera-a11y-composition-grid")]
    SetGrid {
        #[param("Whether the grid is shown")]
        on: bool,
    },
    /// Show level guide
    #[effect(screen, label = "camera-settings-level")]
    SetLevel {
        #[param("Whether the level guide is shown")]
        on: bool,
    },
    /// Mirror front camera
    #[effect(kind = "outside")]
    #[effect(label = "camera-settings-mirror")]
    SetMirrorFront {
        #[param("Whether front-camera photos are mirrored")]
        on: bool,
    },
    /// Tag photo location
    #[effect(kind = "outside")]
    SetGeotag {
        #[param("Whether photos record where they were taken")]
        on: bool,
    },
    /// Use HDR
    #[effect(kind = "outside")]
    #[effect(label = "camera-settings-hdr")]
    SetHdr {
        #[param("Whether photos use HDR")]
        on: bool,
    },
    /// Set default lens
    ///
    /// The lens this device's Camera opens with.
    #[effect(kind = "outside")]
    SetDefaultLens {
        #[param("Lens", choices = ["back", "front", "wide"])]
        lens: String,
    },
    /// Capture photo
    ///
    /// Saves the current frame and answers once the photo is registered.
    #[effect(kind = "outside")]
    #[effect(row, job, cancellable, on = Capture)]
    #[effect(label = "camera-a11y-take-photo")]
    Capture,
    /// Start recording
    ///
    /// Starts a clip, or stops the one recording and answers once it is
    /// saved and registered.
    #[effect(kind = "outside")]
    #[effect(row, job, cancellable, on = Capture)]
    #[effect(label = "camera-a11y-start-recording")]
    Record,
    /// Refresh cameras
    ///
    /// Drops a stale camera session and looks for cameras again.
    #[effect(kind = "outside")]
    #[effect(row, idempotent, on = Capture)]
    RetryCamera,
    /// Open camera privacy settings
    #[effect(kind = "outside")]
    #[effect(row, on = Capture)]
    #[effect(own_device, label = "camera-a11y-camera-privacy")]
    OpenCameraPrivacy,
    /// Use camera
    #[effect(kind = "outside")]
    #[effect(row, on = CameraDevice)]
    UseCamera {
        #[param("The camera's device node, as discovered on this device")]
        device: String,
    },
    /// Scan existing image
    ///
    /// Asks for an image with the file chooser and decodes the QR code or
    /// barcode in it, without importing the image.
    #[effect(kind = "outside")]
    #[effect(attended, row, job, on = Scanner)]
    ImportImage,
    /// Choose a document scan setting
    ///
    /// The scanner, resolution, color, paper source or text language the
    /// next page scans with, by its place in that setting's list.
    #[effect(kind = "outside")]
    ChooseScanSetting {
        #[param("Setting", choices = SCAN_SETTINGS.iter().copied())]
        setting: String,
        #[param("Position of the choice in the setting's list, from 0")]
        choice: String,
    },
    /// Capture document page
    #[effect(kind = "outside")]
    #[effect(row, job, cancellable, on = DocumentScanner)]
    CapturePage,
    /// Rotate selected page
    #[effect(kind = "outside")]
    #[effect(row, on = DocumentScanner)]
    RotatePage,
    /// Recognize text on selected page
    #[effect(kind = "outside")]
    #[effect(row, job, cancellable, on = DocumentScanner)]
    RecognizeText,
    /// Copy recognized text
    #[effect(kind = "outside")]
    #[effect(row, on = DocumentScanner)]
    CopyText,
    /// Export searchable PDF
    ///
    /// Asks where to save the scanned pages as one searchable PDF.
    #[effect(kind = "outside")]
    #[effect(attended, row, job, on = DocumentScanner)]
    ExportPdf,
    /// Crop selected page
    ///
    /// Opens the crop frame over the selected page in this window. Camera's
    /// own view, not the document scanner's: a view on that row would be its
    /// disclosure, a private one that keeps capturing, rotating, recognizing
    /// and exporting pages from agents.
    #[effect(view)]
    CropPage,
    /// Move selected page earlier
    #[effect(kind = "outside")]
    #[effect(row, on = DocumentScanner)]
    MovePageEarlier,
    /// Move selected page later
    #[effect(kind = "outside")]
    #[effect(row, on = DocumentScanner)]
    MovePageLater,
    /// Remove selected page
    #[effect(kind = "outside")]
    #[effect(row, on = DocumentScanner)]
    RemovePage,
}

/// The action a recent capture offers to the app that owns its verified
/// bytes.
pub(crate) const HANDOFF_ACTION: &str = "open";

/// The capture's handoff to the app that owns its verified bytes: the app's
/// typed launch of the content the capture published.
fn capture_handoff(
    is_video: bool,
    content_hash: Option<&str>,
    name: Option<&str>,
) -> Option<ActionSpec> {
    let content_hash = content_hash?;
    let (app_id, destination) = capture_destination(is_video);
    Some(
        ui::framework::ContentHandoff::new(
            app_id,
            format!("Open in {destination}"),
            "Open the verified portable capture in its owning app",
        )
        .spec_named(HANDOFF_ACTION, content_hash, name),
    )
}

/// The handoff the row's type offers, from the content hash and file name the row bound.
pub(crate) fn handoff_spec(is_video: bool, bindings: &RowBindings) -> Option<ActionSpec> {
    let arguments = bindings.arguments(HANDOFF_ACTION)?.first()?;
    let hash = arguments.get("content_hash")?.as_str()?;
    let name = arguments.get("name").and_then(|name| name.as_str());
    capture_handoff(is_video, Some(hash), name)
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
        ("so.tilo.video", "Video")
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
    let kind = if it.is_video {
        CaptureKind::Video
    } else {
        CaptureKind::Photo
    };
    camera_model::capture_resource_id(kind, &it.id)
}

/// The recent capture, offering to open its verified bytes in Photos or
/// Video.
pub(crate) fn recent_row(it: &CaptureItem) -> LiveRow {
    let content_hash = it
        .media_id
        .as_deref()
        .filter(|id| crate::media::is_media_id(id))
        .and_then(|id| id.strip_prefix(crate::media::MEDIA_PREFIX));
    // The handoff is another app's typed launch, offered as the row's own
    // `open`, from the content hash the row carries.
    let mut bindings = RowBindings::new();
    if let Some(hash) = content_hash {
        bindings = bindings.bind_id(
            HANDOFF_ACTION,
            [
                ("content_hash", serde_json::Value::String(hash.to_owned())),
                ("name", serde_json::Value::String(it.id.clone())),
            ],
        );
    }
    let id = capture_resource_id(it);
    let label = item_label(it);
    if it.is_video {
        LiveRow::typed_with::<Video>(id, label, &bindings)
    } else {
        LiveRow::typed_with::<Photo>(id, label, &bindings)
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

fn capture_row(ctx: &CameraResourceCtx) -> LiveRow {
    let bindings = RowBindings::new()
        .offer(CameraEffect::CAPTURE)
        .offer(CameraEffect::RECORD)
        .offer(CameraEffect::RETRY_CAMERA)
        .offer(CameraEffect::OPEN_CAMERA_PRIVACY);
    let mut row = LiveRow::typed_with::<Capture>(CAPTURE_ROW, "Capture", &bindings);
    if !ctx.feed_available && ctx.camera_devices.len() > 1 {
        for device in ctx.camera_devices.iter().take(MAX_PUBLISHED_CAMERA_DEVICES) {
            let label = if ctx.selected_camera.as_deref() == Some(device.as_str()) {
                format!("{device} (selected)")
            } else {
                device.clone()
            };
            let bindings = RowBindings::new().bind(
                CameraEffect::USE_CAMERA,
                [("device", device.clone().into())],
            );
            row = row.row(LiveRow::typed_with::<CameraDevice>(
                camera_device_row_id(device),
                label,
                &bindings,
            ));
        }
    }
    row
}

fn scanner_row(_ctx: &CameraResourceCtx) -> LiveRow {
    let bindings = RowBindings::new().offer(CameraEffect::IMPORT_IMAGE);
    LiveRow::typed_with::<Scanner>(SCANNER_ROW, "Code scanner", &bindings)
}

fn document_row(_ctx: &CameraResourceCtx) -> LiveRow {
    let bindings = RowBindings::new()
        .offer(CameraEffect::CAPTURE_PAGE)
        .offer(CameraEffect::ROTATE_PAGE)
        .offer(CameraEffect::RECOGNIZE_TEXT)
        .offer(CameraEffect::COPY_TEXT)
        .offer(CameraEffect::EXPORT_PDF)
        .offer(CameraEffect::MOVE_PAGE_EARLIER)
        .offer(CameraEffect::MOVE_PAGE_LATER)
        .offer(CameraEffect::REMOVE_PAGE);
    LiveRow::typed_with::<DocumentScanner>(DOCUMENT_ROW, "Document scanner", &bindings)
}

/// What the surface publishes beside the resource's verbs: the capture
/// pipeline with its cameras, the code scanner and the document scanner,
/// each offering its type's effects, and the recent capture with its handoff
/// to the app that owns it. Mode, lens and the capture preferences are
/// effects on the resource.
pub(crate) fn live_rows(ctx: &CameraResourceCtx) -> Vec<LiveRow> {
    let recent = ctx.recent.as_ref().map(recent_row);
    [capture_row(ctx), scanner_row(ctx), document_row(ctx)]
        .into_iter()
        .chain(recent)
        .collect()
}

/// What the camera can do on the row `id` now: an effect it cannot offer
/// says why, and one that reads otherwise than its declaration says (stopping
/// the recording in progress, the camera already chosen) is offered by that
/// name. Served by the surface's `Publishes::row_status`.
pub(crate) fn row_status(ctx: &CameraResourceCtx, id: &str) -> Vec<EffectStatus> {
    if id == CAPTURE_ROW {
        return capture_status(ctx, id);
    }
    if id == SCANNER_ROW {
        return ctx
            .scan_import_pending
            .then(|| {
                EffectStatus::unavailable(
                    CameraEffect::IMPORT_IMAGE,
                    "picker_open",
                    "An image picker is already open",
                )
                .on(id)
            })
            .into_iter()
            .collect();
    }
    if id == DOCUMENT_ROW {
        return document_status(ctx, id);
    }
    device_status(ctx, id)
}

fn capture_status(ctx: &CameraResourceCtx, id: &str) -> Vec<EffectStatus> {
    let mut statuses = Vec::new();
    let photo = if !ctx.feed_available {
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
    };
    if let Some((code, reason)) = photo {
        statuses.push(EffectStatus::unavailable(CameraEffect::CAPTURE, code, reason).on(id));
    }
    // Finalizing an existing clip needs no live camera. Keep the semantic
    // stop api available after disconnection, just like the shutter.
    if ctx.recording {
        statuses.push(
            EffectStatus::labelled(
                CameraEffect::RECORD,
                crate::CATALOG.t("camera-action-stop-recording"),
            )
            .on(id),
        );
    } else if !ctx.feed_available {
        statuses.push(
            EffectStatus::unavailable(
                CameraEffect::RECORD,
                "camera_unavailable",
                "Connect or allow a camera before recording",
            )
            .on(id),
        );
    } else if ctx.mode != CaptureMode::Video {
        statuses.push(
            EffectStatus::unavailable(
                CameraEffect::RECORD,
                "video_mode_required",
                "Switch to video mode before recording",
            )
            .on(id),
        );
    }
    // Recovery is what a camera that is not there needs; with a live feed
    // they stay off the row's way without leaving the row's type.
    if ctx.feed_available {
        statuses.push(
            EffectStatus::unavailable(
                CameraEffect::RETRY_CAMERA,
                "camera_connected",
                "The camera is connected",
            )
            .on(id),
        );
    }
    if ctx.feed_available || !ctx.camera_permission_denied {
        statuses.push(
            EffectStatus::unavailable(
                CameraEffect::OPEN_CAMERA_PRIVACY,
                "camera_allowed",
                "Camera access is allowed on this device",
            )
            .on(id),
        );
    }
    statuses
}

fn device_status(ctx: &CameraResourceCtx, id: &str) -> Vec<EffectStatus> {
    let Some(device) = id.strip_prefix("camera.device:") else {
        return Vec::new();
    };
    let mut statuses = vec![
        EffectStatus::labelled(
            CameraEffect::USE_CAMERA,
            crate::CATALOG.t_args(
                "camera-action-use-device",
                &[("device", ui::i18n::ArgValue::Text(device.to_owned()))],
            ),
        )
        .on(id),
    ];
    if ctx.selected_camera.as_deref() == Some(device) {
        statuses.push(
            EffectStatus::unavailable(
                CameraEffect::USE_CAMERA,
                "already_selected",
                "This camera is already selected",
            )
            .on(id),
        );
    }
    statuses
}

fn document_status(ctx: &CameraResourceCtx, id: &str) -> Vec<EffectStatus> {
    let has_page = ctx.document_pages > 0;
    let effects = [
        (CameraEffect::CAPTURE_PAGE, ctx.document_scanner_available),
        (CameraEffect::ROTATE_PAGE, has_page),
        (CameraEffect::RECOGNIZE_TEXT, has_page),
        (CameraEffect::COPY_TEXT, ctx.document_has_text),
        (CameraEffect::EXPORT_PDF, has_page),
        (
            CameraEffect::MOVE_PAGE_EARLIER,
            has_page && ctx.document_selected > 0,
        ),
        (
            CameraEffect::MOVE_PAGE_LATER,
            has_page && ctx.document_selected + 1 < ctx.document_pages,
        ),
        (CameraEffect::REMOVE_PAGE, has_page),
    ];
    effects
        .into_iter()
        .filter_map(|(effect, available)| {
            let (code, reason) = if ctx.document_busy {
                ("document_busy", "Wait for the current document task")
            } else if available {
                return None;
            } else if effect.id == CameraEffect::CAPTURE_PAGE.id {
                ("no_scanner", "Connect or select a document scanner first")
            } else {
                ("no_selected_page", "Capture and select a page first")
            };
            Some(EffectStatus::unavailable(effect, code, reason).on(id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CameraApp;
    use crate::camera::Camera;
    use ui::tilo_actions::{OperationKind, ResourceSpec};

    fn seeded() -> Camera {
        let mut record = ui::tilo_resource::Replica::<Camera>::new();
        record
            .apply(Camera::REGISTER_CAPTURE.with(crate::resource::sample_capture()))
            .unwrap();
        record.state().unwrap()
    }

    fn tree(ctx: &CameraResourceCtx, state: &Camera) -> Vec<ResourceSpec> {
        let mut resources = vec![ui::tilo_resource::publish::<Camera>(
            "camera", "Camera", state,
        )];
        resources.extend(live_rows(ctx).into_iter().map(LiveRow::into_resource));
        resources
    }

    fn provide_resources(ctx: &CameraResourceCtx) -> Vec<ResourceSpec> {
        tree(ctx, &Camera::default())
    }

    fn find_resource(resource: &ResourceSpec, id: &str) -> Option<ResourceSpec> {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        resource
            .children
            .iter()
            .find_map(|child| find_resource(child, id))
    }

    fn published(state: &Camera) -> Vec<ResourceSpec> {
        let mut app = CameraApp::sample();
        app.present_resource(state);
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
                    Some("so.tilo.photos" | "so.tilo.video")
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
                    .any(|id| { matches!(*id, "so.tilo.photos.app" | "so.tilo.video.app") }),
                "handoff applications are destinations on the current capture, never Camera roots"
            );
            assert!(
                !ids.iter()
                    .any(|id| id.contains("gallery") || id.contains("library"))
            );
        }
        {
            assert_eq!(capture_destination(false), ("so.tilo.photos", "Photos"));
            assert_eq!(capture_destination(true), ("so.tilo.video", "Video"));
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

    tilo_test_env::scenario! {
        fn provided_resources_tell_what_the_camera_can_do_now;

        phase disconnected_camera_keeps_stop_recording_available;
        {
            let ctx = CameraResourceCtx {
                mode: CaptureMode::Video,
                recording: true,
                feed_available: false,
                ..CameraResourceCtx::default()
            };
            assert_eq!(
                row_status(&ctx, CAPTURE_ROW),
                vec![
                    EffectStatus::unavailable(
                        CameraEffect::CAPTURE,
                        "camera_unavailable",
                        "Connect or allow a camera before capturing",
                    )
                    .on(CAPTURE_ROW),
                    EffectStatus::labelled(CameraEffect::RECORD, "Stop recording")
                        .on(CAPTURE_ROW),
                    EffectStatus::unavailable(
                        CameraEffect::OPEN_CAMERA_PRIVACY,
                        "camera_allowed",
                        "Camera access is allowed on this device",
                    )
                    .on(CAPTURE_ROW),
                ],
                "stopping the recording stays available with no feed"
            );
        }

        phase provided_resources_are_honest_about_devices_and_expose_every_preference;
        {
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
                        .all(|spec| spec.id.starts_with("open-")
                            || spec.id.starts_with("use-")
                            || spec.id == "crop-page"),
                    "only the screen, mode and lens reveals and the crop frame are views: {:?}",
                    specs.iter().map(|spec| &spec.id).collect::<Vec<_>>()
                );
            }
            {
                let specs = CameraEffect::specs();
                let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
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
                        ids.contains(&id),
                        "missing preference {id}: {ids:?}"
                    );
                }
                // The grid and the level change only this window, and the
                // privacy sheet opens on the person's own device: neither
                // asks a workspace's grants.
                for id in ["set-grid", "set-level"] {
                    let spec = specs
                        .iter()
                        .find(|spec| spec.id == id)
                        .expect("a capture preference");
                    assert!(
                        spec.contract.only_shows(),
                        "{id} only changes this window's viewfinder"
                    );
                }
                let privacy = CameraEffect::spec("open-camera-privacy")
                    .expect("the privacy sheet");
                assert!(
                    privacy.contract.is_own_device(),
                    "the privacy sheet is of the person's own device"
                );
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
                let ctx = CameraResourceCtx {
                    recent: Some(CaptureItem {
                        id: "IMG_1.jpg".into(),
                        label: "Capture".into(),
                        is_video: false,
                        media_id: None,
                    }),
                    ..CameraResourceCtx::default()
                };
                assert!(
                    row_status(&ctx, CAPTURE_ROW).contains(
                        &EffectStatus::unavailable(
                            CameraEffect::CAPTURE,
                            "camera_unavailable",
                            "Connect or allow a camera before capturing",
                        )
                        .on(CAPTURE_ROW)
                    ),
                    "a palette must not claim capture works without a feed"
                );
                let unverified = resources
                    .iter()
                    .find(|row| row.kind.as_str() == "camera.photo")
                    .expect("recent handoff");
                assert!(unverified.actions.is_empty());

                let ctx = CameraResourceCtx {
                    feed_available: true,
                    ..CameraResourceCtx::default()
                };
                assert!(
                    !row_status(&ctx, CAPTURE_ROW).contains(
                        &EffectStatus::unavailable(
                            CameraEffect::CAPTURE,
                            "camera_unavailable",
                            "Connect or allow a camera before capturing",
                        )
                        .on(CAPTURE_ROW)
                    ),
                    "a live feed offers capture"
                );
                assert!(
                    row_status(&ctx, CAPTURE_ROW).contains(
                        &EffectStatus::unavailable(
                            CameraEffect::RETRY_CAMERA,
                            "camera_connected",
                            "The camera is connected",
                        )
                        .on(CAPTURE_ROW)
                    ),
                    "recovery is offered only when the camera is not there"
                );
            }
            {
                let ctx = CameraResourceCtx {
                    camera_permission_denied: true,
                    camera_devices: vec!["/dev/video0".into(), "/dev/video2".into()],
                    ..CameraResourceCtx::default()
                };
                let resources = provide_resources(&ctx);
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
                    row.actions
                        .iter()
                        .find(|action| action.id == "use-camera")
                        .expect("a camera row offers choosing it");
                    let statuses = row_status(&ctx, camera_device_row_id(device).as_str());
                    assert!(
                        statuses.contains(
                            &EffectStatus::labelled(
                                CameraEffect::USE_CAMERA,
                                format!("Use {device}"),
                            )
                            .on(camera_device_row_id(device)),
                        ),
                        "{device} offers choosing it: {statuses:?}"
                    );
                    assert!(
                        statuses.iter().all(|status| status
                            != &EffectStatus::unavailable(
                                CameraEffect::USE_CAMERA,
                                "already_selected",
                                "This camera is already selected",
                            )
                            .on(camera_device_row_id(device))),
                        "nothing is chosen yet"
                    );
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
}
