//! File Explorer as a [`tilo_resource::Resource`].
//!
//! Folder presentation, places Open, templates, and filesystem jobs are
//! View/Effect operations published from this resource via
//! [`tilo_resource::publish`]. View preferences and bookmark paths are
//! document actions. Path-scoped live listing rows stay on the surface, and
//! are the only extras [`PublishesResources`] returns when hosted.
//! [`FileExplorer::children`] still loads places from disk.

use std::rc::Rc;

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, ParamType,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::doc::ViewDoc;
use crate::files_app::{
    FileExplorerWidget, folder_palette_operations, load_published_places,
    published_place_resources, published_template_resources,
};

/// Collaborative view preferences and favourite paths.
///
/// Listing rows, the current directory, and selection are not in this
/// vocabulary — they stay on the widget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesDocAction {
    SetView {
        mode: String,
    },
    SetSort {
        key: String,
        order: String,
    },
    SetZoom {
        level: i32,
    },
    SetHidden {
        shown: bool,
    },
    /// Ordered favourite paths: places and bookmarks.
    SetBookmarks {
        paths: Vec<String>,
    },
}

impl Action for FilesDocAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetView { mode } => ViewDoc::set_view_mode(doc, mode),
            Self::SetSort { key, order } => ViewDoc::set_sort(doc, key, order),
            Self::SetZoom { level } => ViewDoc::set_zoom(doc, *level),
            Self::SetHidden { shown } => ViewDoc::set_show_hidden(doc, *shown),
            Self::SetBookmarks { paths } => ViewDoc::set_bookmarks(doc, paths),
        }
        Ok(())
    }
}

/// View projection stored in the shared document.
#[derive(Clone, Debug)]
pub struct FilesState {
    pub view_mode: String,
    pub sort_key: String,
    pub sort_order: String,
    pub zoom: i32,
    pub show_hidden: bool,
    pub bookmarks: Vec<String>,
    /// False when the document has no shared view map yet.
    pub shared: bool,
}

impl Default for FilesState {
    fn default() -> Self {
        Self {
            view_mode: "list".into(),
            sort_key: "name".into(),
            sort_order: "ascending".into(),
            zoom: 0,
            show_hidden: false,
            bookmarks: Vec::new(),
            shared: false,
        }
    }
}

/// Current-folder resource. Kind matches the published folder root.
pub struct FileExplorer;

impl Resource for FileExplorer {
    const KIND: &'static str = "files.folder";
    const LABEL: &'static str = "Folder";
    const ACTION_TYPE: &'static str = "files.action";

    type State = FilesState;
    type Action = FilesDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<FilesDocAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<FilesState, ModelError> {
        Ok(FilesState {
            view_mode: ViewDoc::view_mode(doc),
            sort_key: ViewDoc::sort_key(doc),
            sort_order: ViewDoc::sort_order(doc),
            zoom: ViewDoc::zoom(doc),
            show_hidden: ViewDoc::show_hidden(doc),
            bookmarks: ViewDoc::bookmarks(doc),
            shared: ViewDoc::is_shared(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let mut declarations = view_declarations();
        declarations.extend(bookmark_declarations());
        declarations
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Path-independent folder Views/Effects. Live listing rows and remote
        // accounts stay on the surface; places/templates are children below.
        folder_palette_operations()
    }

    fn children(_state: &FilesState) -> Vec<tilo_actions::Resource> {
        // Disk places stay here. Document bookmark edits do not replace them.
        let mut children = published_place_resources(&load_published_places());
        children.extend(published_template_resources());
        children
    }
}

fn view_declarations() -> Vec<ActionDecl<FileExplorer>> {
    vec![
        ActionDecl::new(
            "set-view",
            "Set view",
            vec![
                ActionParam::required("mode", ParamType::Text, "list | grid")
                    .with_choices(["list", "grid"]),
            ],
            |args, _| {
                let mode = args
                    .text("mode")
                    .ok_or_else(|| "missing view mode".to_owned())?
                    .to_owned();
                Ok(FilesDocAction::SetView { mode })
            },
        )
        .describe("Switch the shared list or grid view."),
        ActionDecl::new(
            "set-sort",
            "Set sort",
            vec![
                ActionParam::required("key", ParamType::Text, "name | size | type | modified")
                    .with_choices(["name", "size", "type", "modified"]),
                ActionParam::required("order", ParamType::Text, "ascending | descending")
                    .with_choices(["ascending", "descending"]),
            ],
            |args, _| {
                let key = args
                    .text("key")
                    .ok_or_else(|| "missing sort key".to_owned())?
                    .to_owned();
                let order = args
                    .text("order")
                    .ok_or_else(|| "missing sort order".to_owned())?
                    .to_owned();
                Ok(FilesDocAction::SetSort { key, order })
            },
        )
        .describe("Set the shared folder sort."),
        ActionDecl::new(
            "set-zoom",
            "Set item size",
            vec![ActionParam::required(
                "level",
                ParamType::Integer,
                "Zoom step",
            )],
            |args, _| {
                let level = args
                    .integer("level")
                    .ok_or_else(|| "missing zoom level".to_owned())?;
                let level =
                    i32::try_from(level).map_err(|_| "zoom level is out of range".to_owned())?;
                Ok(FilesDocAction::SetZoom { level })
            },
        )
        .describe("Set the shared item size."),
        ActionDecl::new(
            "set-hidden",
            "Show hidden files",
            vec![ActionParam::required(
                "shown",
                ParamType::Boolean,
                "Whether dotfiles are listed",
            )],
            |args, _| {
                let shown = args
                    .boolean("shown")
                    .ok_or_else(|| "missing hidden flag".to_owned())?;
                Ok(FilesDocAction::SetHidden { shown })
            },
        )
        .describe("Set whether the shared view lists hidden files."),
    ]
}

fn bookmark_declarations() -> Vec<ActionDecl<FileExplorer>> {
    vec![
        ActionDecl::new(
            "set-bookmarks",
            "Set bookmarks",
            vec![ActionParam::required(
                "paths",
                ParamType::Array,
                "Favourite folder paths, in order",
            )],
            |args, _| {
                let paths = args
                    .array("paths")
                    .ok_or_else(|| "missing bookmark paths".to_owned())?
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect();
                Ok(FilesDocAction::SetBookmarks { paths })
            },
        )
        .describe("Replace the shared places and bookmarks order."),
    ]
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
    fn present(&mut self, state: &Rc<FilesState>) {
        // Adopt shared view prefs and bookmarks only. Path, selection,
        // clipboard, and the live listing stay process-local.
        self.adopt_shared_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<FileExplorer>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone,
/// `--shots`, and unit tests.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<FileExplorer>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<FileExplorer>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_hosted(&mut self, hosted: bool) {
        self.hosted = hosted;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &FilesDocAction) {
        let context = ui::tilo_actions::ActionContext::for_action(
            FileExplorerShell::ID,
            FileExplorerShell::TITLE,
            action,
        );
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(FileExplorer::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => {
                self.local
                    .dispatch_with_context(doc, FileExplorer::ACTION_TYPE, action, context)
            }
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

/// Live launch on the framework host.
#[cfg(not(target_arch = "wasm32"))]
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
        let published =
            ui::tilo_resource::publish::<FileExplorer>("files", "Folder", &FilesState::default());
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

    #[test]
    fn hosted_document_edit_is_submitted_through_the_sink_without_a_second_folder_root() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"x").unwrap();

        let mut widget = FileExplorerWidget::new();
        widget.navigate(directory.path().to_path_buf());
        let mut doc = AutoCommit::new();
        assert!(
            !widget.is_framework_hosted(),
            "standalone stays on the local host"
        );
        widget.set_view_mode(crate::model::ViewMode::Grid);
        widget.sync(&mut doc, &ui::automerge::ROOT);
        assert!(
            widget.last_shared_action().is_none(),
            "an unhosted view change stays on the local widget"
        );
        assert!(
            !ViewDoc::is_shared(&doc),
            "an unhosted view change must not write the shared document"
        );

        widget.set_view_mode(crate::model::ViewMode::List);
        widget.set_framework_hosted(true);
        assert!(widget.is_framework_hosted());
        widget.set_view_mode(crate::model::ViewMode::Grid);
        widget.sync(&mut doc, &ui::automerge::ROOT);

        let submitted = widget
            .last_shared_action()
            .expect("hosted edit submitted through the sink");
        assert_eq!(submitted.type_id, FileExplorer::ACTION_TYPE);
        let action: FilesDocAction =
            serde_json::from_slice(&submitted.payload).expect("sink payload is the document edit");
        assert_eq!(
            action,
            FilesDocAction::SetView {
                mode: "grid".into(),
            }
        );
        assert_eq!(ViewDoc::view_mode(&doc), "grid");

        let page = PublishesResources::action_directory_resource_page(&widget, &doc, &[], 8)
            .expect("hosted directory");
        assert!(
            page.resources
                .iter()
                .all(|resource| resource.kind != "files.folder"),
            "hosted directory must not include a second folder root: {:?}",
            page.resources
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        let extras = widget.resources(&doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "files.folder"),
            "hosted resources must not rebuild the folder root: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "files.entry" && resource.label == "notes.txt"),
            "live listing rows stay on the widget: {:?}",
            extras
                .iter()
                .map(|resource| (resource.kind.as_str(), resource.label.as_str()))
                .collect::<Vec<_>>()
        );
    }
}
