//! Imager as a [`tilo_resource::Resource`]: install profile is document state;
//! write/verify jobs are Effects in [`Resource::local_operations`].

use std::rc::Rc;

use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    IMAGER_ACTION_TYPE, ImagerAction, document_children, export_config_spec, operation_resource,
    register,
};
use crate::app::ImagerApp;
use crate::doc::ImagerDoc;
use crate::model::{InstallProfile, Source};

/// The reviewed image, the images reviewed before it and the install
/// profile: everything the imager keeps in its document.
#[derive(Clone, Debug, Default)]
pub struct ImagerState {
    pub source: Option<Source>,
    /// Previously reviewed images, most recent first.
    pub recent: Vec<Source>,
    /// The rank a newly remembered image takes: before every recent one.
    pub next_recent_rank: i64,
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
            recent: ImagerDoc::recent(doc),
            next_recent_rank: ImagerDoc::next_recent_rank(doc),
            profile: ImagerDoc::profile(doc),
        })
    }

    /// An empty reviewed source, recent list and install profile.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        ImagerDoc::seed(doc);
        Ok(())
    }

    /// Typing into one profile field is one step until the field changes.
    fn coalesces(previous: &ImagerAction, next: &ImagerAction) -> bool {
        previous.same_text_field(next)
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
        self.present_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        self.attach_framework(cx.dispatcher());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_publish_owns_profile_door_and_hosted_keeps_no_duplicate_root() {
        let mut doc = automerge::AutoCommit::new();
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
    }
}
