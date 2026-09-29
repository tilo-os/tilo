//! Disk Usage as a [`tilo_resource::Resource`]: scan jobs are Effects; recent
//! directories are the only shared projection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::{DiskUsageApp, DiskUsageOperation, operation_action};
use crate::doc::DiskUsageDoc;

/// No collaborative document mutations beyond the recent-path projection,
/// which the surface writes when a scan completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
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
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
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
        ]
    }
}

impl ResourceSurface<DiskUsage> for DiskUsageApp {
    /// A scan in progress is this machine's. An empty recent list must not
    /// clear it.
    fn present(&mut self, _state: &std::rc::Rc<DiskUsageState>) {}

    fn attach(&mut self, _cx: SurfaceCx<DiskUsage>) {
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
}
