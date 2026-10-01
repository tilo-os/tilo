//! File Explorer as a [`tilo_resource::Resource`]: no replayable CRDT actions
//! (see [`crate::actions`]); folder presentation, places Open, templates, and
//! filesystem jobs are View/Effect operations published from this resource.
//! Path-scoped live listing rows stay on the surface when not framework-hosted,
//! and are the only extras [`PublishesResources`] returns when hosted.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::files_app::{
    FileExplorerWidget, folder_palette_operations, load_published_places,
    published_place_resources, published_template_resources,
};

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
        // Path-independent folder Views/Effects. Live listing rows and remote
        // accounts stay on the surface; places/templates are children below.
        folder_palette_operations()
    }

    fn children(_state: &()) -> Vec<tilo_actions::Resource> {
        let mut children = published_place_resources(&load_published_places());
        children.extend(published_template_resources());
        children
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
    fn present(&mut self, _state: &Rc<()>) {
        // Empty on purpose: Files holds path, selection, clipboard and rename
        // draft as process-local state. Wiping that here would drop unsynced
        // local interaction that never lived in the Automerge document.
    }

    fn attach(&mut self, _cx: SurfaceCx<FileExplorer>) {
        self.set_framework_hosted(true);
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<FileExplorerShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_places_and_folder_ops() {
        let published = ui::tilo_resource::publish::<FileExplorer>("files", "Folder", &());
        assert_eq!(published.kind, "files.folder");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "refresh",
            "new-folder",
            "paste",
            "go-to",
            "up",
            "back",
            "forward",
            "sort-name",
            "list",
            "grid",
            "add-favorite",
            "remove-favorite",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride local_operations via publish: {ids:?}"
            );
        }
        let refresh = published
            .actions
            .iter()
            .find(|action| action.id == "refresh")
            .expect("refresh");
        assert_eq!(refresh.kind, OperationKind::Effect);
        assert_eq!(refresh.operation_type.as_deref(), Some("files.refresh"));
        let go_to = published
            .actions
            .iter()
            .find(|action| action.id == "go-to")
            .expect("go-to");
        assert_eq!(go_to.kind, OperationKind::View);

        let place_children: Vec<_> = published
            .children
            .iter()
            .filter(|child| child.kind == "files.place")
            .collect();
        assert!(
            !place_children.is_empty(),
            "publish must advertise place Open children: {:?}",
            published
                .children
                .iter()
                .map(|c| c.kind.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            place_children
                .iter()
                .all(|place| place.actions.iter().any(|a| a.id == "open")),
            "each place child carries Open"
        );

        let mut widget = FileExplorerWidget::new();
        let mut doc = AutoCommit::new();
        widget.sync(&mut doc, &ui::automerge::ROOT);
        assert!(
            !widget.is_framework_hosted(),
            "standalone stays on the local host"
        );
        let standalone = widget.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "files.folder");
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|a| a.id == "refresh" || a.id == "paste"),
            "standalone still publishes the folder root"
        );

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"x").unwrap();
        widget.navigate(directory.path().to_path_buf());
        widget.sync(&mut doc, &ui::automerge::ROOT);

        widget.set_framework_hosted(true);
        assert!(widget.is_framework_hosted());
        let folder_id = crate::search_provider::folder_resource_id(directory.path());
        assert!(ui::resources::resource(&widget, &doc, &folder_id).is_none());
        let place_id = place_children[0].id.as_str().to_owned();
        assert!(
            ui::resources::resource(&widget, &doc, &place_id)
                .is_some_and(|place| place.actions.iter().any(|action| action.id == "open")),
            "a hosted place stays the published row"
        );
        let extras = widget.resources(&doc);
        assert!(
            extras.iter().all(|r| r.kind != "files.folder"),
            "hosted must not republish files.folder beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|r| r.kind == "files.entry" && r.label == "notes.txt"),
            "hosted still publishes live listing rows: {:?}",
            extras
                .iter()
                .map(|r| (r.kind.as_str(), r.label.as_str()))
                .collect::<Vec<_>>()
        );
        let entry = extras
            .iter()
            .find(|r| r.kind == "files.entry")
            .expect("entry row");
        let entry_ids: Vec<_> = entry.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in ["open", "rename", "trash", "copy", "cut"] {
            assert!(
                entry_ids.contains(&expected),
                "live entry missing `{expected}`: {entry_ids:?}"
            );
        }
        assert!(
            extras.iter().all(|r| r.kind != "files.place"),
            "places ride publish, not hosted extras"
        );
    }
}
