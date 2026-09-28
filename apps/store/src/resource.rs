//! App Store as a [`tilo_resource::Resource`]: report drafts are document
//! state; install/update jobs are Effects published by the surface.

use automerge::{ROOT, ReadDoc};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::ActionSpec;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::app::StoreApp;
use crate::doc_actions::{
    REPORT_RESOURCE_KEY_PREFIX, STORE_DOCUMENT_ACTION_TYPE, StoreDocumentAction, register,
};

/// Report drafts persisted in the shared document.
#[derive(Clone, Debug, Default)]
pub struct StoreState {
    pub report_app_ids: Vec<String>,
}

/// The app-store catalogue resource.
pub struct Store;

impl Resource for Store {
    const KIND: &'static str = "store.catalog";
    const LABEL: &'static str = "App Store";
    const ACTION_TYPE: &'static str = STORE_DOCUMENT_ACTION_TYPE;

    type State = StoreState;
    type Action = StoreDocumentAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<StoreState, ModelError> {
        let mut report_app_ids: Vec<String> = doc
            .keys(&ROOT)
            .filter_map(|key| {
                key.strip_prefix(REPORT_RESOURCE_KEY_PREFIX)
                    .map(str::to_owned)
            })
            .collect();
        report_app_ids.sort();
        Ok(StoreState { report_app_ids })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Install / update / remote Effects are bound to catalogue items the
        // surface resolves from the host; they are not view-independent.
        Vec::new()
    }

    fn search(state: &StoreState) -> Vec<SearchEntry> {
        state
            .report_app_ids
            .iter()
            .map(|app_id| SearchEntry {
                anchor: Some(format!("store.report:{app_id}")),
                title: format!("Report · {app_id}"),
                text: String::new(),
            })
            .collect()
    }
}

/// Framework app.
pub struct StoreShell;

impl ResourceApp for StoreShell {
    type Resource = Store;
    const ID: &'static str = "so.tilo.store";
    const TITLE: &'static str = "App Store";
    const ICON: Option<&'static str> = Some("store");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Store>) -> Element {
        cx.surface(StoreApp::new())
    }
}

impl ResourceSurface<Store> for StoreApp {
    fn present(&mut self, _state: &std::rc::Rc<StoreState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Store>) {}
}
