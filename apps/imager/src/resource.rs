//! Imager as a [`tilo_resource::Resource`]: install profile is document state;
//! write/verify jobs are Effects in [`Resource::local_operations`].

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    IMAGER_ACTION_TYPE, ImagerAction, document_children, export_config_spec, operation_resource,
    register,
};
use crate::app::ImagerApp;
use crate::doc::ImagerDoc;
use crate::model::{InstallProfile, Source};

/// Shared image-selection and install-profile projection.
#[derive(Clone, Debug, Default)]
pub struct ImagerState {
    pub source: Option<Source>,
    pub profile: InstallProfile,
}

/// The disk-imager sources / write-plan resource.
pub struct Imager;

impl Resource for Imager {
    const KIND: &'static str = "imager.sources";
    const LABEL: &'static str = "Image sources";
    const ACTION_TYPE: &'static str = IMAGER_ACTION_TYPE;

    type State = ImagerState;
    type Action = ImagerAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ImagerState, ModelError> {
        Ok(ImagerState {
            source: ImagerDoc::source(doc),
            profile: ImagerDoc::profile(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Profile field ActionSpecs are state-gated (only an authenticated
        // installable Tilo image) and live on the customization child from
        // [`children`]. Typed declarations here would advertise them for a
        // plain ISO too.
        Vec::new()
    }

    fn bound_actions(state: &ImagerState) -> Vec<ActionSpec> {
        // Clear-source is bound on the reviewed source child in [`children`].
        let _ = state;
        Vec::new()
    }

    fn children(state: &ImagerState) -> Vec<tilo_actions::Resource> {
        document_children(state.source.as_ref(), &state.profile)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Device / job availability is machine-local; publish a conservative
        // baseline here. Standalone surfaces overlay live write availability on
        // the same root via [`overlay_live_write_ops`].
        let mut ops = operation_resource(None, false, false, crate::write::write_available())
            .actions
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect::<Vec<_>>();
        ops.push(export_config_spec(false));
        ops
    }
}

/// Replace the conservative write-plan Effects on a published root with the
/// live availability for this surface. Same resource id — not a second tree.
pub(crate) fn overlay_live_write_ops(root: &mut tilo_actions::Resource, app: &ImagerApp) {
    const LIVE: &[&str] = &["write", "verify", "cancel", "status", "export-config"];
    root.actions
        .retain(|spec| !LIVE.contains(&spec.id.as_str()));
    let available = app.resource_source().is_some() && app.target().is_some();
    let mut job = operation_resource(
        app.target()
            .map(crate::model::TargetDrive::dev_path)
            .as_deref(),
        available,
        !app.startup_write_reconciled() || app.job_active(),
        crate::write::write_available(),
    );
    job.actions.push(export_config_spec(available));
    for action in job.actions {
        if matches!(action.kind, OperationKind::View | OperationKind::Effect) {
            root.actions.push(action);
        }
    }
}

/// Hosted-only extras that are not on the framework resource. Job progress and
/// probed targets are widget/machine state; status/write Effects already live
/// on [`Imager::local_operations`], so hosted surfaces publish nothing extra.
pub(crate) fn live_extras(_app: &ImagerApp) -> Vec<tilo_actions::Resource> {
    Vec::new()
}

/// Framework app.
pub struct ImagerShell;

impl ResourceApp for ImagerShell {
    type Resource = Imager;
    const ID: &'static str = "so.tilo.imager";
    const TITLE: &'static str = "Disk Imager";
    const ICON: Option<&'static str> = Some("imager");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1180, 820)
    }

    fn view(cx: &mut ViewCx<Imager>) -> Element {
        cx.surface(ImagerApp::new())
    }
}

impl ResourceSurface<Imager> for ImagerApp {
    fn present(&mut self, state: &Rc<ImagerState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for the selected image and install profile.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Imager>>,
    hosted: bool,
    last: Option<EncodedAction>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Imager>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &ImagerAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(IMAGER_ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => {
                self.local
                    .dispatch_with_context(doc, type_id, action, context);
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_profile_door_and_hosted_keeps_no_duplicate_root() {
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        let state = Imager::read(&doc).expect("seeded imager state");
        let published =
            ui::tilo_resource::publish::<Imager>("imager.sources", "Image sources", &state);
        assert_eq!(published.kind, "imager.sources");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "write" && action.kind == OperationKind::Effect),
            "write Effect must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "export-config"),
            "export-config View must be on the framework resource"
        );
        let source = published
            .children
            .iter()
            .find(|child| child.kind == "imager.source")
            .expect("reviewed source child");
        assert!(
            source
                .actions
                .iter()
                .any(|action| action.id == "clear-source"),
            "clear-source must ride the source child"
        );
        let customization = published
            .children
            .iter()
            .find(|child| child.kind == "imager.customization")
            .expect("customization child must carry the install-profile palette");
        assert!(
            customization
                .actions
                .iter()
                .any(|action| action.id == "hostname"),
            "hostname must be on the customization child"
        );

        let mut app = ImagerApp::new();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "imager.sources");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "imager.customization"),
            "standalone publishes the profile tree"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "imager.sources").is_none());
        let source_id = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "imager.source")
            .map(|child| child.id.as_str().to_owned())
            .expect("reviewed source");
        assert!(
            ui::resources::resource(&app, &seeded, &source_id).is_some_and(|source| source
                .actions
                .iter()
                .any(|action| action.id == "clear-source")),
            "a hosted image source stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras.is_empty(),
            "hosted must not republish imager.sources beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
    }
}
