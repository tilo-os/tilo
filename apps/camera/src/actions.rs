//! Camera's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! The camera's *pixels* are ordinary files on disk — a shutter press writes a
//! PNG/JPEG, a recording writes a clip — and the *device I/O* around them
//! (opening a lens, decoding a QR out of a frame, the live viewfinder) is
//! external, not collaborative. What lives in the shared Automerge document
//! (see [`crate::doc`]) is the *metadata*: which gallery shots are favorited and
//! the history of scanned codes. Those two are the camera's collaborative CRUD,
//! so those two are its [`CameraAction`]s: small serializable values that
//! deterministically mutate the CRDT and therefore replicate, replay and undo
//! for free. The exact same [`EncodedAction`] the gallery's favorite affordance
//! dispatches is what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! ## What is *not* an action
//!
//! Capturing a photo/video (the frame write is device I/O; the file lands via
//! FUSE), deleting the file itself (device I/O — only its favorite flag is a
//! doc mutation), the camera settings (resolution / format / timer / flash /
//! lens — device and session state), and the live preview are all external,
//! never actions. Which shot is selected, the gallery scroll, the active filter,
//! zoom, and the photo/video mode switch are presentation and stay out of the
//! document too.
//!
//! ## Determinism contract
//!
//! Both variants carry everything [`Action::apply`] needs (the item id, the
//! favorite direction, the scanned payload, and a timestamp minted at authoring
//! time); `apply` never touches the RNG or the wall-clock. A malformed payload
//! (an empty id, an empty scan) is a **no-op** rather than an error, so a bad
//! replay can't wedge a peer.

use automerge::{AutoCommit, ROOT};
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceProvider,
};

use crate::doc;
use crate::model::{CaptureMode, Facing};

/// Registry `type_id` for every Camera action.
pub const CAMERA_ACTION_TYPE: &str = "camera.action";
pub const CAMERA_OPERATION_TYPE: &str = "camera.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraOperation {
    Capture,
    ToggleRecording,
    Share { id: String },
    Delete { id: String },
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
    SetMode {
        mode: CaptureMode,
    },
    SetFacing {
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
            CameraAction::SetMode { mode } => doc::write_mode(doc, &ROOT, *mode),
            CameraAction::SetFacing { facing } => doc::write_facing(doc, &ROOT, *facing),
            CameraAction::SetFavorite { id, on } => {
                // An empty id is never produced by our own builder; no-op.
                if !id.is_empty() {
                    doc::set_favorite(doc, &ROOT, id, *on);
                }
            }
            CameraAction::RecordScan { raw, ts } => {
                if !raw.is_empty() {
                    doc::record_scan(doc, &ROOT, raw, *ts);
                }
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
}

impl Default for CameraResourceCtx {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            recording: false,
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
pub fn provide_resources(ctx: &CameraResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("camera.gallery", "camera.gallery", "Gallery");
    if let Ok(spec) = ActionSpec::operation(
        "capture",
        "Capture photo",
        OperationKind::Effect,
        CAMERA_OPERATION_TYPE,
        &CameraOperation::Capture,
    ) {
        root = root.with_action(if ctx.mode == CaptureMode::Photo && !ctx.recording {
            spec
        } else {
            spec.unavailable(
                "photo_mode_required",
                "Switch to photo mode before capturing",
            )
        });
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
        root = root.with_action(if ctx.mode == CaptureMode::Video {
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
        // File deletion also clears collaborative favourite metadata. The
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
    vec![root]
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
        assert_eq!(resources.len(), 1, "one gallery root");
        let gallery = &resources[0];
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
        let delete = resources[0].children[0]
            .actions
            .iter()
            .find(|a| a.id == "delete-file")
            .unwrap();
        assert!(delete.contract.availability.is_available());
        assert_eq!(delete.contract.risk, qos_actions::RiskLevel::Destructive);
    }
}
