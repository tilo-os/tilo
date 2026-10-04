//! Disk Usage as a [`tilo_resource::Resource`]: scan jobs are Effects; recent
//! directories are the shared projection. A finished scan submits
//! [`DiskUsageAction::Remember`] through the framework dispatcher when hosted,
//! and through the local action host for standalone, `--shots`, and unit tests.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::{DiskUsageApp, DiskUsageOperation, operation_action};
use crate::doc::DiskUsageDoc;

/// Remember a directory the local scan just finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiskUsageAction {
    Remember { path: String },
}

impl Action for DiskUsageAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        let Self::Remember { path } = self;
        DiskUsageDoc::remember(doc, std::path::Path::new(path));
        Ok(())
    }
}

/// Recent directories remembered in the shared document.
#[derive(Clone, Debug, Default)]
pub struct DiskUsageState {
    pub recent: Vec<PathBuf>,
}

/// The disk-usage analyzer resource.
pub struct DiskUsage;

impl Resource for DiskUsage {
    const KIND: &'static str = "disk-usage";
    const LABEL: &'static str = "Disk Usage Analyzer";
    const ACTION_TYPE: &'static str = "disk_usage.action";

    type State = DiskUsageState;
    type Action = DiskUsageAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<DiskUsageAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<DiskUsageState, ModelError> {
        Ok(DiskUsageState {
            recent: DiskUsageDoc::recent(doc),
        })
    }

    fn children(state: &Self::State) -> Vec<ui::tilo_actions::Resource> {
        state
            .recent
            .iter()
            .filter_map(|path| {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let mut resource = ui::tilo_actions::Resource::new(
                    format!("disk-usage.recent:{}", path.display()),
                    "disk-usage.recent",
                    &name,
                );
                resource.actions.push(operation_action(
                    &format!("analyze:{}", path.display()),
                    &format!("Analyze {name}"),
                    &DiskUsageOperation::Analyze { path: path.clone() },
                )?);
                Some(resource)
            })
            .collect()
    }

    fn local_operations() -> Vec<ActionSpec> {
        [
            ("choose", "Choose folder", DiskUsageOperation::Choose),
            ("home", "Show locations", DiskUsageOperation::Home),
            ("back", "Go back", DiskUsageOperation::Back),
        ]
        .into_iter()
        .filter_map(|(id, label, op)| operation_action(id, label, &op))
        .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
        .collect()
    }
}

/// Framework app.
pub struct DiskUsageShell;

impl ResourceApp for DiskUsageShell {
    type Resource = DiskUsage;
    const ID: &'static str = "so.tilo.disk-usage";
    const TITLE: &'static str = "Disk Usage Analyzer";
    const ICON: Option<&'static str> = Some("disk-usage");
    const SHOT_VIEWPORT: (u32, u32) = (1180, 760);

    fn view(cx: &mut ViewCx<DiskUsage>) -> Element {
        cx.surface(DiskUsageApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "analysis",
            "analysis-narrow",
            "analysis-hover",
            "analysis-hover-narrow",
            "home",
            "home-narrow",
            "loading",
            "error",
        ]
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<DiskUsage>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<DiskUsage>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted || self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut ui::automerge::AutoCommit,
        type_id: &str,
        action: &DiskUsageAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<ui::automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&ui::tilo_actions::EncodedAction> {
        self.local.last_dispatched()
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

impl ResourceSurface<DiskUsage> for DiskUsageApp {
    /// A scan in progress is this machine's. An empty recent list must not
    /// clear it.
    fn present(&mut self, _state: &std::rc::Rc<DiskUsageState>) {}

    fn attach(&mut self, cx: SurfaceCx<DiskUsage>) {
        self.actions.attach(cx.dispatcher());
        self.mark_framework_hosted();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ui::resources::PublishesResources;

    #[test]
    fn publish_owns_navigation_and_hosted_keeps_folder_rows() {
        let recent = DiskUsageState {
            recent: vec![PathBuf::from("/tmp/kept")],
        };
        let published =
            ui::tilo_resource::publish::<DiskUsage>("disk-usage", "Disk Usage Analyzer", &recent);
        let ids: Vec<&str> = published
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for expected in ["choose", "home", "back"] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish: {ids:?}"
            );
        }
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "disk-usage.recent"),
            "recent directories ride publish children"
        );

        let mut home = DiskUsageApp::new();
        assert!(!home.is_framework_hosted());
        let standalone = home.resources(&ui::automerge::AutoCommit::new());
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, DiskUsage::KIND);

        home.mark_hosted_for_test();
        let places = home.resources(&ui::automerge::AutoCommit::new());
        assert!(
            places
                .iter()
                .all(|resource| resource.kind != DiskUsage::KIND),
            "hosted must not republish disk-usage"
        );
        assert!(
            places
                .iter()
                .any(|resource| resource.kind == "disk-usage.folder")
        );

        let mut analysis = DiskUsageApp::sample();
        assert!(analysis.in_analysis());
        ResourceSurface::<DiskUsage>::present(
            &mut analysis,
            &std::rc::Rc::new(DiskUsageState::default()),
        );
        assert!(
            analysis.in_analysis(),
            "empty present must not wipe a local scan"
        );
        analysis.mark_hosted_for_test();
        let folders = analysis.resources(&ui::automerge::AutoCommit::new());
        assert!(folders.len() > places.len());
        assert!(
            folders
                .iter()
                .all(|resource| resource.kind == "disk-usage.folder")
        );
    }

    #[test]
    fn hosted_remember_is_recorded_without_republishing_root() {
        let mut app = DiskUsageApp::new();
        assert!(!app.is_framework_hosted());
        assert!(!app.sink_is_hosted());
        let mut doc = ui::automerge::AutoCommit::new();
        let standalone = app.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, DiskUsage::KIND);

        app.mark_hosted_for_test();
        assert!(app.is_framework_hosted());
        assert!(app.sink_is_hosted());
        let path = PathBuf::from("/tmp/kept");
        app.remember_scanned(&mut doc, &path);
        let recorded = app
            .last_dispatched()
            .expect("hosted remember is recorded on the action sink");
        assert_eq!(recorded.type_id.as_str(), DiskUsage::ACTION_TYPE);
        assert_eq!(DiskUsageDoc::recent(&doc), vec![path]);
        let extras = app.resources(&doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != DiskUsage::KIND
                    && resource.id.as_str() != "disk-usage:root"),
            "hosted must not republish disk-usage beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "disk-usage.folder"),
            "places stay on the widget"
        );
    }
}
