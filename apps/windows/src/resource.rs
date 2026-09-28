//! Windows Apps as a [`tilo_resource::Resource`]: route overrides are document
//! state; install/VM jobs are Effects published by the surface.

use automerge::{ObjType, ROOT, ReadDoc, Value};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{WINDOWS_ACTION_TYPE, WindowsAction, provide_resources, register};
use crate::app::WindowsApp;
use crate::model::{Catalogue, Route};

/// Collaborative route overrides keyed by app id.
#[derive(Clone, Debug, Default)]
pub struct WindowsState {
    pub routes: Vec<(String, Route)>,
}

/// The Windows compatibility manager resource.
pub struct Windows;

impl Resource for Windows {
    const KIND: &'static str = "windows.manager";
    const LABEL: &'static str = "Windows manager";
    const ACTION_TYPE: &'static str = WINDOWS_ACTION_TYPE;

    type State = WindowsState;
    type Action = WindowsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<WindowsState, ModelError> {
        let mut routes = Vec::new();
        if let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(ROOT, "windows-compat") {
            for app_id in doc.keys(&map) {
                if let Some(route) = crate::actions::route_override(doc, &app_id) {
                    routes.push((app_id, route));
                }
            }
        }
        routes.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(WindowsState { routes })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    fn local_operations() -> Vec<ActionSpec> {
        let empty = Catalogue {
            apps: Vec::new(),
            vm: crate::model::ApplianceState {
                ready: false,
                detail: String::new(),
                running_apps: 0,
                shared_session_active: false,
                shared_session_grants: Vec::new(),
                shared_session_contributors: Vec::new(),
            },
            vm_install_confirmation: None,
        };
        provide_resources(&empty)
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &WindowsState) -> Vec<SearchEntry> {
        state
            .routes
            .iter()
            .map(|(app_id, route)| SearchEntry {
                anchor: Some(format!("windows.app:{app_id}")),
                title: app_id.clone(),
                text: route.label().to_owned(),
            })
            .collect()
    }
}

/// Framework app.
pub struct WindowsShell;

impl ResourceApp for WindowsShell {
    type Resource = Windows;
    const ID: &'static str = "so.tilo.windows";
    const TITLE: &'static str = "Windows Apps";
    const ICON: Option<&'static str> = Some("windows");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Windows>) -> Element {
        cx.surface(WindowsApp::new_live())
    }
}

impl ResourceSurface<Windows> for WindowsApp {
    fn present(&mut self, _state: &std::rc::Rc<WindowsState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Windows>) {}
}
