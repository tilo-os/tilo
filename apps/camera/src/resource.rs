//! Camera as a [`tilo_resource::Resource`]: one action vocabulary for capture
//! prefs and scan history; live shutter / device Effects stay extras.
//!
//! Document mutations (mode, lens, settings toggles) live on this resource via
//! [`Resource::bound_actions`] / [`Resource::children`]. Screen reveals are
//! Views ([`Resource::local_operations`]). Shutter, record, device chooser,
//! code-scanner import and document-scanner Effects are hosted live extras —
//! never a second hand-built Document tree beside [`tilo_resource::publish`].

use std::rc::Rc;

use automerge::{AutoCommit, ROOT};
use ui::framework::{Command, Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    CAMERA_ACTION_TYPE, CameraAction, bound_mode_facing_actions, capture_item_from_shared,
    live_effect_extras, publish_document_children, register, screen_view_specs,
};
use crate::app::CameraApp;
use crate::doc::{self, SharedCapture};
use crate::model::{CaptureMode, Facing, ScanEntry, Settings};

/// Stable id for the published camera root.
pub const CAMERA_RESOURCE_ID: &str = "camera";

/// Shared camera document projection.
#[derive(Clone, Debug)]
pub struct CameraState {
    pub mode: CaptureMode,
    pub facing: Facing,
    pub settings: Settings,
    pub scans: Vec<ScanEntry>,
    pub recent: Option<SharedCapture>,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            mode: CaptureMode::Photo,
            facing: Facing::Back,
            settings: Settings::default(),
            scans: Vec::new(),
            recent: None,
        }
    }
}

/// The camera resource.
pub struct Camera;

impl Resource for Camera {
    const KIND: &'static str = "camera";
    const LABEL: &'static str = "Camera";
    const ACTION_TYPE: &'static str = CAMERA_ACTION_TYPE;

    type State = CameraState;
    type Action = CameraAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<CameraState, ModelError> {
        Ok(CameraState {
            mode: doc::read_mode(doc, &ROOT),
            facing: doc::read_facing(doc, &ROOT),
            settings: doc::read_settings(doc, &ROOT),
            scans: doc::read_scans(doc, &ROOT),
            recent: doc::read_recent_capture(doc, &ROOT),
        })
    }

    fn bound_actions(state: &CameraState) -> Vec<ActionSpec> {
        // Recording is a live Effect; hosted publish cannot see it, so mode
        // stays available here and the surface still refuses while recording.
        bound_mode_facing_actions(state.facing, false)
            .into_iter()
            .filter(|spec| spec.kind == OperationKind::Document)
            .collect()
    }

    fn children(state: &CameraState) -> Vec<PublishedResource> {
        let recent = state.recent.as_ref().map(capture_item_from_shared);
        publish_document_children(&state.settings, recent.as_ref(), &state.scans)
    }

    fn local_operations() -> Vec<ActionSpec> {
        screen_view_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &CameraState) -> Vec<SearchEntry> {
        let mut entries = Vec::new();
        if let Some(recent) = &state.recent {
            entries.push(SearchEntry {
                anchor: Some(format!("camera.capture:{}", recent.id)),
                title: recent.label.clone(),
                text: "recent capture".into(),
                action: None,
            });
        }
        for scan in &state.scans {
            entries.push(SearchEntry {
                anchor: Some(format!("camera.scan:{}", scan.ts)),
                title: scan.raw.clone(),
                text: "scan".into(),
                action: None,
            });
        }
        entries
    }
}

/// Hosted-only extras that are not document state.
pub(crate) fn live_extras(app: &CameraApp) -> Vec<PublishedResource> {
    live_effect_extras(&app.camera_resource_ctx())
}

/// Framework app: the camera widget is the surface over [`Camera`].
pub struct CameraShell;

impl ResourceApp for CameraShell {
    type Resource = Camera;
    const ID: &'static str = "so.tilo.camera";
    const TITLE: &'static str = "Camera";
    const ICON: Option<&'static str> = Some("camera");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Camera>) -> Element {
        cx.surface(CameraApp::disk())
    }

    fn commands() -> &'static [Command] {
        &[Command {
            option: "--emit-photos",
            run: emit_photos,
        }]
    }
}

/// `--emit-photos DIR`: write procedural photos and a lossless QR poster
/// into `DIR`, the library a headless test drives the camera against.
fn emit_photos(dir: &str) -> Result<(), String> {
    use crate::model::PhotoFormat;
    let dir = std::path::Path::new(dir);
    for seed in 0..5u8 {
        let frame = crate::frame::thumbnail(seed, 960, 640);
        crate::library::write_photo(dir, &frame, PhotoFormat::Jpeg)
            .map_err(|error| error.to_string())?;
        let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(4));
    }
    let (qr, _) = crate::frame::scanner_frame(
        4,
        900,
        640,
        "WIFI:T:WPA2;S:Tilo Lounge;P:welcome-aboard;H:false;;",
    );
    crate::library::write_photo(dir, &qr, PhotoFormat::Png).map_err(|error| error.to_string())?;
    println!("camera: wrote sample photos to {}", dir.display());
    Ok(())
}

impl ResourceSurface<Camera> for CameraApp {
    fn present(&mut self, state: &Rc<CameraState>) {
        // Adopt shared prefs / scans / handoff only — never wipe unpublished local.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Camera>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for capture settings, scans, and the recent handoff.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Camera>>,
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Camera>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &CameraAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = crate::actions::encode(action).ok();
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Hosted commits are recorded by the dispatcher. Standalone tests still
    /// read the local host's rising edge.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<CameraShell>()
}

pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots"
            | "--responsive-matrix"
            | "--native-audit"
            | "--headless"
            | "--scan-first"
            | "--disk"
            | "--crop"
            | "--timer-running" => return true,
            "--screen" | "--mode" | "--dir" | "--scroll" | "--hover" | "--width" | "--height"
            | "--camera-state" => {
                let _ = args.next();
                return true;
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::Action;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one assertion covers publish ownership and hosted extras"
    )]
    fn resource_publish_owns_document_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        CameraAction::SetMode {
            mode: CaptureMode::Video,
        }
        .apply(&mut doc)
        .unwrap();
        CameraAction::SetGrid { on: true }.apply(&mut doc).unwrap();
        doc.commit();

        let state = Camera::read(&doc).expect("seeded camera state");
        let published = ui::tilo_resource::publish::<Camera>(CAMERA_RESOURCE_ID, "Camera", &state);
        assert_eq!(published.kind, "camera");
        for id in ["photo-mode", "video-mode", "lens-back", "lens-front"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id.starts_with("open-")),
            "screen reveal Views stay on the published door"
        );
        let settings = published
            .children
            .iter()
            .find(|child| child.kind == "camera.settings")
            .expect("settings child on the published door");
        for id in ["grid", "resolution-4k", "hdr"] {
            assert!(
                settings.actions.iter().any(|action| action.id == id
                    || action.id.starts_with("resolution-")
                    || action.id == "hdr"
                    || action.id == "grid"),
                "{id}-family must be on settings; got {:?}",
                settings
                    .actions
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            settings.actions.iter().any(|action| action.id == "grid"),
            "grid Document op must be on settings"
        );

        let mut app = CameraApp::sample();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        let camera = standalone
            .iter()
            .find(|resource| resource.kind == "camera")
            .expect("standalone publishes the camera root");
        assert!(
            camera
                .children
                .iter()
                .any(|child| child.kind == "camera.settings"),
            "settings ride the published camera"
        );
        assert!(
            camera
                .actions
                .iter()
                .any(|action| action.id == "video-mode"),
            "mode Document ops ride the published camera"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras.iter().all(|resource| {
                !matches!(resource.kind.as_str(), "camera.settings" | "camera")
                    && resource
                        .actions
                        .iter()
                        .all(|action| action.kind != OperationKind::Document)
            }),
            "hosted must not republish Document prefs beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|r| {
                    (
                        r.id.as_str(),
                        r.actions
                            .iter()
                            .map(|a| (a.id.as_str(), a.kind))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        );
        assert!(
            extras.iter().any(|r| r.kind == "camera.capture"),
            "hosted still publishes live capture Effects"
        );

        // Empty present must not wipe unpublished local prefs / scans.
        let mut local = CameraApp::sample();
        let mut local_doc = AutoCommit::new();
        local.sync(&mut local_doc, &ROOT);
        let before_scans = local.scan_count_for_test();
        local.adopt_resource_state(&CameraState::default());
        assert_eq!(
            local.scan_count_for_test(),
            before_scans,
            "adopting an empty projection must not clear unpublished scans"
        );
    }
}
