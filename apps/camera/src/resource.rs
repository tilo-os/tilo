//! Camera as a [`tilo_resource::Resource`]: capture settings and the recent
//! handoff that sync are document state; viewfinder presentation is View.

use std::rc::Rc;

use automerge::{AutoCommit, ROOT};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{CAMERA_ACTION_TYPE, CameraAction, CameraResourceCtx, register};
use crate::app::CameraApp;
use crate::doc::{self, SharedCapture};
use crate::model::{CaptureMode, Facing, ScanEntry, Settings};

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

    fn children(state: &CameraState) -> Vec<tilo_actions::Resource> {
        let mut children = Vec::new();
        if let Some(recent) = &state.recent {
            children.push(tilo_actions::Resource::new(
                format!("camera.capture:{}", recent.id),
                "camera.capture-item",
                recent.label.clone(),
            ));
        }
        for scan in state.scans.iter().take(32) {
            children.push(tilo_actions::Resource::new(
                format!("camera.scan:{}", scan.ts),
                "camera.scan",
                scan.raw.clone(),
            ));
        }
        children
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::provide_resources(
            &CameraResourceCtx::default(),
            &automerge::AutoCommit::new(),
        )
        .into_iter()
        .flat_map(|resource| resource.actions)
        .filter(|spec| spec.kind != OperationKind::Document)
        .collect()
    }

    fn search(state: &CameraState) -> Vec<SearchEntry> {
        let mut entries = Vec::new();
        if let Some(recent) = &state.recent {
            entries.push(SearchEntry {
                anchor: Some(format!("camera.capture:{}", recent.id)),
                title: recent.label.clone(),
                text: "recent capture".into(),
            });
        }
        for scan in &state.scans {
            entries.push(SearchEntry {
                anchor: Some(format!("camera.scan:{}", scan.ts)),
                title: scan.raw.clone(),
                text: "scan".into(),
            });
        }
        entries
    }
}

/// Framework app: the camera widget is the surface over [`Camera`].
pub struct CameraShell;

impl ResourceApp for CameraShell {
    type Resource = Camera;
    const ID: &'static str = "so.tilo.camera";
    const TITLE: &'static str = "Camera";
    const ICON: Option<&'static str> = Some("camera");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Camera>) -> Element {
        cx.surface(CameraApp::disk())
    }
}

impl ResourceSurface<Camera> for CameraApp {
    fn present(&mut self, state: &Rc<CameraState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Camera>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for capture settings, scans, and the recent handoff.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Camera>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Camera>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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
            | "--timer-running"
            | "--emit-photos" => return true,
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
