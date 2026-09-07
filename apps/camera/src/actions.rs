//! Camera's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! The camera's *pixels* are verified qdoc blobs with an optional local file
//! projection — a shutter press writes a PNG/JPEG, a recording writes a clip
//! — and the *device I/O* around them
//! (opening a lens, decoding a QR out of a frame, the live viewfinder) is
//! external, not collaborative. What lives in the shared Automerge document
//! (see [`crate::doc`]) is the *metadata*: portable capture registrations,
//! mode and capture preferences, favorites, and scan history. Those durable
//! choices are [`CameraAction`]s: small serializable values that
//! deterministically mutate the CRDT and therefore replicate, replay and undo
//! for free. The exact same [`EncodedAction`] the gallery's favorite affordance
//! dispatches is what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! ## What is *not* an action
//!
//! Capturing a photo/video (the frame write is device I/O), deleting the local
//! file projection (device I/O — shared metadata removal is a canonical
//! action), and the live preview are external effects. Which shot is selected, the gallery
//! scroll, the active filter, zoom, and an open chooser are presentation-only.
//!
//! ## Determinism contract
//!
//! Both variants carry everything [`Action::apply`] needs (the item id, the
//! favorite direction, the scanned payload, and a timestamp minted at authoring
//! time); `apply` never touches the RNG or the wall-clock. A malformed payload
//! or an oversized legacy list is an explicit [`ActionError::Refused`], so the
//! invoker never mistakes a rejected durable mutation for success.

use automerge::{AutoCommit, ROOT};
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceProvider,
};

use crate::doc;
use crate::model::{
    CaptureKind, CaptureMode, Facing, PhotoFormat, Resolution, Screen, Settings, Timer,
};

/// Registry `type_id` for every Camera action.
pub const CAMERA_ACTION_TYPE: &str = "camera.action";
pub const CAMERA_OPERATION_TYPE: &str = "camera.operation";

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
    Share {
        id: String,
    },
    Delete {
        id: String,
    },
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

/// Every collaborative operation Camera exports on its shared gallery/scan
/// metadata. The pixels and the decode are external; these are the doc writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraAction {
    /// Atomically publish a bounded capture metadata row pointing only at
    /// verified qdoc bytes. This is the peer-replayable capture boundary.
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
    /// Remove a published capture from the shared gallery index. Blob bytes
    /// are content-addressed and may remain referenced elsewhere; this action
    /// removes only this resource's canonical metadata row.
    RemoveCapture {
        id: String,
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
    /// Star / unstar a gallery shot by id (its file name). The pixels live on
    /// disk (external); only the collaborative "favorited" bit is this action.
    /// Explicit (`on`) rather than a toggle so a delete can also clear the flag
    /// unconditionally, and so replay is a plain deterministic set.
    SetFavorite {
        id: String,
        on: bool,
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
            } => doc::register_shared_capture(
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
            CameraAction::RemoveCapture { id } => {
                doc::remove_shared_capture(doc, &ROOT, id).map_err(ActionError::Refused)?;
            }
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
            CameraAction::SetFavorite { id, on } => {
                // An empty id is never produced by our own builder; no-op.
                if !id.is_empty() {
                    doc::set_favorite(doc, &ROOT, id, *on);
                }
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

pub fn set_favorite_action(id: &str, on: bool) -> CameraAction {
    CameraAction::SetFavorite {
        id: id.to_string(),
        on,
    }
}

pub fn record_scan_action(raw: &str, ts: i64) -> CameraAction {
    CameraAction::RecordScan {
        raw: raw.to_string(),
        ts,
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// One gallery shot surfaced as a resource: its id (file name), a one-line
/// label, its current favorite state, and whether it is a video. Everything
/// here is derived from state the app already holds; nothing is fabricated.
#[derive(Debug, Clone)]
pub struct GalleryItem {
    pub id: String,
    pub label: String,
    pub favorite: bool,
    pub is_video: bool,
}

/// The presentation context Camera hands [`provide_resources`]: the gallery
/// shots currently in the library cache (itself re-projected from disk / the
/// sample source, with favorite state from the document).
#[derive(Debug, Clone)]
pub struct CameraResourceCtx {
    pub items: Vec<GalleryItem>,
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
}

impl Default for CameraResourceCtx {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            recording: false,
            feed_available: false,
            camera_permission_denied: false,
            camera_devices: Vec::new(),
            selected_camera: None,
            settings: Settings::default(),
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

fn item_label(it: &GalleryItem) -> String {
    let l = it.label.trim();
    let base = if l.is_empty() { it.id.as_str() } else { l };
    if it.is_video {
        format!("{base} (video)")
    } else {
        base.to_string()
    }
}

/// Build the resource tree Camera currently has open: the gallery, with one
/// resource per shot exporting the favorite / unfavorite action. The direction
/// is chosen from the shot's *current* favorite state (recomputed each frame —
/// a steady, level-triggered value), so the on-screen heart and the palette row
/// dispatch the identical [`EncodedAction`].
#[allow(
    clippy::too_many_lines,
    reason = "the gallery and settings are Camera's two resource roots; keeping their complete, typed action inventory together makes the palette contract auditable"
)]
pub fn provide_resources(ctx: &CameraResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut navigation = Resource::new("camera", "camera", "Camera");
    for screen in [
        Screen::Camera,
        Screen::Scanner,
        Screen::Gallery,
        Screen::Settings,
    ] {
        if let Ok(spec) = ActionSpec::operation(
            format!("open-{}", screen.as_str()),
            format!("Open {}", screen.label()),
            OperationKind::View,
            CAMERA_OPERATION_TYPE,
            &CameraOperation::Reveal { screen },
        ) {
            navigation = navigation.with_action(spec);
        }
    }
    let mut root = Resource::new("camera.gallery", "camera.gallery", "Gallery");
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
            for device in &ctx.camera_devices {
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
    for (id, label, mode) in [
        ("photo-mode", "Use photo mode", CaptureMode::Photo),
        ("video-mode", "Use video mode", CaptureMode::Video),
    ] {
        if let Some(spec) = immediate_spec(id, label, &CameraAction::SetMode { mode }) {
            root = root.with_action(if ctx.recording {
                spec.unavailable("recording_active", "Stop recording before changing mode")
            } else {
                spec
            });
        }
    }
    for facing in Facing::ALL {
        if let Some(spec) = immediate_spec(
            &format!("lens-{}", facing.as_str()),
            &format!("Use {} lens", facing.label()),
            &CameraAction::SetFacing { facing },
        ) {
            root = root.with_action(if facing == ctx.facing {
                spec.unavailable("already_selected", "This lens is already selected")
            } else {
                spec
            });
        }
    }
    for it in &ctx.items {
        let kind = if it.is_video {
            "camera.video"
        } else {
            "camera.photo"
        };
        // Explicit set toward the opposite of the current state: favoriting an
        // unstarred shot, unfavoriting a starred one.
        let (label, on) = if it.favorite {
            ("Unfavorite", false)
        } else {
            ("Favorite", true)
        };
        let mut child = Resource::new(format!("{kind}:{}", it.id), kind, item_label(it));
        if let Some(spec) = immediate_spec("favorite", label, &set_favorite_action(&it.id, on)) {
            child = child.with_action(spec);
        }
        if let Ok(spec) = ActionSpec::operation(
            "share",
            "Share capture",
            OperationKind::Effect,
            CAMERA_OPERATION_TYPE,
            &CameraOperation::Share { id: it.id.clone() },
        ) {
            child = child.with_action(spec);
        }
        // File deletion also removes collaborative capture/favourite metadata. The
        // host's compound-operation boundary gives Camera the document only
        // for this admission turn, so deleting through Search/Ctrl+K follows
        // the same replayable metadata path as the on-screen Delete control.
        if let Ok(spec) = ActionSpec::operation(
            "delete-file",
            "Delete capture from disk",
            OperationKind::Effect,
            CAMERA_OPERATION_TYPE,
            &CameraOperation::Delete { id: it.id.clone() },
        ) {
            child = child.with_action(spec.destructive());
        }
        root = root.with_child(child);
    }
    let mut settings = Resource::new("camera.settings", "camera.settings", "Capture settings");
    for resolution in Resolution::ALL {
        if let Some(spec) = immediate_spec(
            &format!("resolution-{}", resolution.as_str()),
            &format!("Use {} resolution", resolution.label()),
            &CameraAction::SetResolution { resolution },
        ) {
            settings = settings.with_action(if resolution == ctx.settings.resolution {
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
            settings = settings.with_action(if photo_format == ctx.settings.photo_format {
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
            settings = settings.with_action(if timer == ctx.settings.timer {
                spec.unavailable("already_selected", "This timer is already selected")
            } else {
                spec
            });
        }
    }
    for (id, label, action) in [
        (
            "grid",
            if ctx.settings.grid {
                "Turn off composition grid"
            } else {
                "Turn on composition grid"
            },
            CameraAction::SetGrid {
                on: !ctx.settings.grid,
            },
        ),
        (
            "level",
            if ctx.settings.level {
                "Turn off level guide"
            } else {
                "Turn on level guide"
            },
            CameraAction::SetLevel {
                on: !ctx.settings.level,
            },
        ),
        (
            "mirror-front",
            if ctx.settings.mirror_front {
                "Turn off front-camera mirroring"
            } else {
                "Turn on front-camera mirroring"
            },
            CameraAction::SetMirrorFront {
                on: !ctx.settings.mirror_front,
            },
        ),
        (
            "geotag",
            if ctx.settings.geotag {
                "Turn off photo location"
            } else {
                "Turn on photo location"
            },
            CameraAction::SetGeotag {
                on: !ctx.settings.geotag,
            },
        ),
        (
            "hdr",
            if ctx.settings.hdr {
                "Turn off HDR"
            } else {
                "Turn on HDR"
            },
            CameraAction::SetHdr {
                on: !ctx.settings.hdr,
            },
        ),
    ] {
        if let Some(spec) = immediate_spec(id, label, &action) {
            settings = settings.with_action(spec);
        }
    }
    for facing in Facing::ALL {
        if let Some(spec) = immediate_spec(
            &format!("default-lens-{}", facing.as_str()),
            &format!("Use {} as the default camera", facing.label()),
            &CameraAction::SetDefaultFacing { facing },
        ) {
            settings = settings.with_action(if facing == ctx.settings.default_facing {
                spec.unavailable(
                    "already_selected",
                    "This default camera is already selected",
                )
            } else {
                spec
            });
        }
    }
    // Keep the gallery first for callers that scope Ctrl+K to the current
    // media collection, then expose navigation as its own searchable root.
    vec![root, settings, navigation]
}

impl ResourceProvider for crate::app::CameraApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.camera_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CameraApp;
    use automerge::transaction::Transactable;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    /// A hermetic Camera app with its sample gallery synced into a fresh
    /// document. The sample set carries both photos and videos, some favorited.
    fn seeded() -> (CameraApp, AutoCommit) {
        let mut app = CameraApp::sample();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// Ctrl+K on Camera yields the gallery tree: a `camera.gallery` root with a
    /// `camera.photo` / `camera.video` child per shot, each exporting the
    /// favorite action. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_gallery_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(
            resources.len(),
            3,
            "gallery, settings, and navigation roots"
        );
        let gallery = resources
            .iter()
            .find(|resource| resource.id.as_str() == "camera.gallery")
            .expect("gallery root");
        assert_eq!(gallery.kind, "camera.gallery");
        assert!(!gallery.children.is_empty(), "the sample gallery has shots");

        let photo = gallery
            .children
            .iter()
            .find(|r| r.kind == "camera.photo")
            .expect("a photo resource");
        let ids: Vec<&str> = photo.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"favorite"), "{ids:?}");
        assert!(photo.actions[0].contract.reversible);
        assert!(
            gallery.children.iter().any(|r| r.kind == "camera.video"),
            "a video resource"
        );

        // The one palette surfaces the favorite rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels
                .iter()
                .any(|l| l.contains("Favorite") || l.contains("Unfavorite")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn favorite_action_records_one_undo_boundary_and_reverts() {
        let (_app, seeded_doc) = seeded();
        let mut state = ui::State::new();
        state.doc = seeded_doc;

        // A sample shot that starts unfavorited (see `sample_captures`).
        let id = "c2";
        assert!(!doc::read_favorites(&state.doc, &ROOT).contains(id));
        let before_heads = state.doc.get_heads();

        let action = set_favorite_action(id, true);
        let encoded = encode(&action).unwrap();
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            doc::read_favorites(&state.doc, &ROOT).contains(id),
            "the action favorited the shot"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            !doc::read_favorites(&state.doc, &ROOT).contains(id),
            "undo reverted the favorite and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded favorite on a
    /// second independent replica (from the same base state) lands on the
    /// byte-identical document — same head hashes, not merely the same values.
    #[test]
    fn favorite_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = set_favorite_action("c5", true);
        let encoded = encode(&action).unwrap();
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            doc::read_favorites(&b, &ROOT).contains("c5"),
            "the favorite landed on the peer too"
        );
    }

    /// A scan-history row-insert also converges byte-identically — the dedup +
    /// cap runs deterministically against the shared base on each replica.
    #[test]
    fn record_scan_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = record_scan_action("https://qos.example/hello", 1234);
        let encoded = encode(&action).unwrap();
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "scan replayed identically");
        let scans = doc::read_scans(&b, &ROOT);
        assert_eq!(
            scans.first().map(|s| s.raw.as_str()),
            Some("https://qos.example/hello"),
            "the scan landed at the front of the peer's history"
        );
    }

    #[test]
    fn capture_preferences_are_granular_replayable_actions() {
        let mut a = AutoCommit::new();
        doc::init_defaults(&mut a, &ROOT, crate::model::Screen::Camera);
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let action = CameraAction::SetGrid { on: false };
        let encoded = encode(&action).unwrap();
        let envelope = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &envelope, &registry()).unwrap();

        assert!(!doc::read_settings(&a, &ROOT).grid);
        assert!(!doc::read_settings(&b, &ROOT).grid);
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "the preference replayed exactly"
        );
    }

    #[test]
    fn published_capture_replays_as_portable_metadata() {
        let media_id = crate::media::store(b"portable camera bytes").unwrap();
        let mut a = AutoCommit::new();
        doc::init_defaults(&mut a, &ROOT, Screen::Camera);
        a.commit();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let action = CameraAction::RegisterCapture {
            id: "IMG_peer.jpg".into(),
            media_id,
            kind: crate::model::CaptureKind::Photo,
            ts: 42,
            w: 64,
            h: 48,
            dur_ms: 0,
            label: "Peer photo".into(),
            facing: Facing::Back,
        };
        let envelope = qos_actions::apply_local(&mut a, &action, encode(&action).unwrap()).unwrap();
        qos_actions::replay(&mut b, &envelope, &registry()).unwrap();
        assert_eq!(a.get_heads(), b.get_heads());
        let captures = doc::read_shared_captures(&b, &ROOT);
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].id, "IMG_peer.jpg");
        assert!(crate::media::is_media_id(&captures[0].media_id));
    }

    #[test]
    fn published_capture_removal_replays_without_touching_blob_bytes() {
        let media_id = crate::media::store(b"portable camera bytes").unwrap();
        let mut a = AutoCommit::new();
        doc::init_defaults(&mut a, &ROOT, Screen::Camera);
        let register = CameraAction::RegisterCapture {
            id: "IMG_remove.jpg".into(),
            media_id: media_id.clone(),
            kind: crate::model::CaptureKind::Photo,
            ts: 42,
            w: 64,
            h: 48,
            dur_ms: 0,
            label: "Remove me".into(),
            facing: Facing::Back,
        };
        qos_actions::apply_local(&mut a, &register, encode(&register).unwrap()).unwrap();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let remove = CameraAction::RemoveCapture {
            id: "IMG_remove.jpg".into(),
        };
        let envelope = qos_actions::apply_local(&mut a, &remove, encode(&remove).unwrap()).unwrap();
        qos_actions::replay(&mut b, &envelope, &registry()).unwrap();
        assert!(doc::read_shared_captures(&a, &ROOT).is_empty());
        assert!(doc::read_shared_captures(&b, &ROOT).is_empty());
        assert_eq!(
            crate::media::read(&media_id).as_deref(),
            Some(b"portable camera bytes".as_slice())
        );
    }

    #[test]
    fn oversized_capture_index_is_an_explicit_action_refusal() {
        let mut doc = AutoCommit::new();
        let list = doc
            .put_object(&ROOT, "captures", automerge::ObjType::List)
            .expect("capture list");
        for index in 0..=2_000 {
            doc.insert(&list, index, "legacy row")
                .expect("legacy capture row");
        }
        let action = CameraAction::RemoveCapture {
            id: "IMG_refuse.jpg".into(),
        };

        assert!(matches!(
            action.apply(&mut doc),
            Err(ActionError::Refused(message)) if message.contains("safety limit")
        ));
    }

    #[test]
    fn navigation_is_a_local_reveal_operation_and_discoverable_resource() {
        let resources = provide_resources(&CameraResourceCtx::default(), &AutoCommit::new());
        let navigation = resources
            .iter()
            .find(|resource| resource.id.as_str() == "camera")
            .expect("camera navigation resource");
        assert!(
            navigation
                .actions
                .iter()
                .any(|spec| spec.id == "open-scanner")
        );
        let scanner = navigation
            .actions
            .iter()
            .find(|spec| spec.id == "open-scanner")
            .expect("scanner reveal action");
        assert_eq!(scanner.kind, OperationKind::View);
    }

    #[test]
    fn ctrl_k_exposes_every_capture_preference() {
        let resources = provide_resources(&CameraResourceCtx::default(), &AutoCommit::new());
        let settings = resources
            .iter()
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

    /// Single source of truth: the on-screen favorite affordance dispatches the
    /// byte-identical [`EncodedAction`] its palette [`ActionSpec`] exports.
    /// Driven through the real app handler, not a helper that fabricates the
    /// action — the button's actual dispatch is captured and compared.
    #[test]
    fn favorite_button_and_spec_encode_identically() {
        let (mut app, mut doc) = seeded();
        // The gallery's first shot and the palette spec built for its *current*
        // favorite state.
        let resources = Widget::resources(&app, &doc);
        let item = resources[0]
            .children
            .first()
            .expect("a gallery item")
            .clone();
        let id = item.id.as_str().rsplit(':').next().unwrap().to_string();
        let spec = item.actions.iter().find(|a| a.id == "favorite").unwrap();
        let from_spec = immediate_encode(spec);

        // Drive the real favorite affordance and capture what it dispatched.
        app.test_select(&id);
        app.test_toggle_favorite(&mut doc);
        let from_button = app
            .test_last_dispatched()
            .expect("the button dispatched an action");

        assert_eq!(from_spec.type_id, from_button.type_id);
        assert_eq!(
            from_spec.payload, from_button.payload,
            "one source of truth"
        );
    }

    #[test]
    fn device_effects_are_canonical_and_delete_is_destructive() {
        let resources = provide_resources(
            &CameraResourceCtx {
                items: vec![GalleryItem {
                    id: "IMG_1.jpg".into(),
                    label: "Capture".into(),
                    favorite: false,
                    is_video: false,
                }],
                ..CameraResourceCtx::default()
            },
            &AutoCommit::new(),
        );
        let capture = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "capture")
            .unwrap();
        assert_eq!(capture.kind, OperationKind::Effect);
        assert!(
            !capture.contract.availability.is_available(),
            "a palette must not claim capture works without a feed"
        );
        let delete = resources[0].children[0]
            .actions
            .iter()
            .find(|a| a.id == "delete-file")
            .unwrap();
        assert!(delete.contract.availability.is_available());
        assert_eq!(delete.contract.risk, qos_actions::RiskLevel::Destructive);

        let available = provide_resources(
            &CameraResourceCtx {
                feed_available: true,
                ..CameraResourceCtx::default()
            },
            &AutoCommit::new(),
        );
        let capture = available[0]
            .actions
            .iter()
            .find(|action| action.id == "capture")
            .unwrap();
        assert!(capture.contract.availability.is_available());
    }

    #[test]
    fn unavailable_camera_publishes_real_recovery_operations() {
        let resources = provide_resources(
            &CameraResourceCtx {
                camera_permission_denied: true,
                camera_devices: vec!["/dev/video0".into(), "/dev/video2".into()],
                ..CameraResourceCtx::default()
            },
            &AutoCommit::new(),
        );
        let actions = &resources[0].actions;
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
