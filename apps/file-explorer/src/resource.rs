//! File Explorer as a [`tilo_resource::Resource`]: no replayable CRDT actions
//! (see [`crate::actions`]); folder presentation and filesystem jobs are
//! View/Effect operations. Path-scoped catalogue rows still need the live
//! folder listing, so the surface publishes them when not framework-hosted.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::files_app::FileExplorerWidget;

/// Empty document vocabulary — Files does not mint CRDT mutations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilesDocAction {}

impl Action for FilesDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Current-folder resource. Kind matches the published folder root.
pub struct FileExplorer;

impl Resource for FileExplorer {
    const KIND: &'static str = "files.folder";
    const LABEL: &'static str = "Folder";
    const ACTION_TYPE: &'static str = "files.action";

    type State = ();
    type Action = FilesDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<FilesDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<(), ModelError> {
        Ok(())
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Path-independent folder jobs. Path-bound entry rows stay on the
        // surface until the shared filesystem file resource owns them.
        [
            (
                "refresh",
                "Refresh folder",
                OperationKind::Effect,
                "files.refresh",
            ),
            (
                "new-folder",
                "New folder",
                OperationKind::Effect,
                "files.new-folder",
            ),
        ]
        .into_iter()
        .filter_map(|(id, label, kind, type_id)| {
            ActionSpec::operation(id, label, kind, type_id, &"").ok()
        })
        .collect()
    }
}

/// Framework app: the files widget is the surface over [`FileExplorer`].
pub struct FileExplorerShell;

impl ResourceApp for FileExplorerShell {
    type Resource = FileExplorer;
    const ID: &'static str = "so.tilo.file-explorer";
    const TITLE: &'static str = "Files";
    const ICON: Option<&'static str> = Some("file-explorer");
    const SHOT_VIEWPORT: (u32, u32) = (1200, 720);

    fn view(cx: &mut ViewCx<FileExplorer>) -> Element {
        cx.surface(FileExplorerWidget::new())
    }
}

impl ResourceSurface<FileExplorer> for FileExplorerWidget {
    fn present(&mut self, _state: &Rc<()>) {}

    fn attach(&mut self, _cx: SurfaceCx<FileExplorer>) {
        self.set_framework_hosted(true);
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<FileExplorerShell>()
}
