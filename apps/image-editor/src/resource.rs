//! Image Editor as a [`tilo_resource::Resource`]: the project graph is the
//! state, adjustments are document actions, and the editor widget is the
//! surface.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{IMAGE_EDITOR_ACTION_TYPE, ImageEditorAction, register};
use crate::app::ImageEditorApp;
use crate::doc::ProjectDoc;
use crate::model::Project;

/// The image project resource.
pub struct ImageEditor;

impl Resource for ImageEditor {
    const KIND: &'static str = "image-editor.project";
    const LABEL: &'static str = "Image Editor";
    const ACTION_TYPE: &'static str = IMAGE_EDITOR_ACTION_TYPE;

    type State = Project;
    type Action = ImageEditorAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Project, ModelError> {
        Ok(ProjectDoc::read(doc))
    }

    fn children(state: &Project) -> Vec<tilo_actions::Resource> {
        crate::actions::project_children(state)
    }

    /// Add-adjustment Document ops whose payload mints a stable node id at
    /// publish time. They ride `tilo_resource::publish`, not a second hand-built
    /// root beside `ResourceRoot`.
    fn bound_actions(state: &Project) -> Vec<ActionSpec> {
        crate::actions::project_bound_document_actions(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::project_local_operations()
    }

    fn search(state: &Project) -> Vec<SearchEntry> {
        state
            .graph
            .nodes
            .iter()
            .filter(|(id, _)| *id != "source" && *id != "output")
            .map(|(id, node)| SearchEntry {
                anchor: Some(crate::actions::adjustment_resource_id(id)),
                title: node.title.clone(),
                text: id.clone(),
            })
            .collect()
    }
}

/// Framework app: the editor widget is the surface over [`ImageEditor`].
pub struct ImageEditorShell;

impl ResourceApp for ImageEditorShell {
    type Resource = ImageEditor;
    const ID: &'static str = "so.tilo.image-editor";
    const TITLE: &'static str = "Image Editor";
    const ICON: Option<&'static str> = Some("image-editor");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<ImageEditor>) -> Element {
        cx.surface(ImageEditorApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        crate::SHOT_SCENARIOS
    }

    fn label(state: &Project) -> String {
        if state.name.is_empty() {
            "Image Editor".into()
        } else {
            state.name.clone()
        }
    }
}

impl ResourceSurface<ImageEditor> for ImageEditorApp {
    fn present(&mut self, state: &Rc<Project>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<ImageEditor>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<ImageEditor>>,
    last: Option<EncodedAction>,
    /// Test override for [`Self::is_hosted`] without a real dispatcher.
    #[cfg(test)]
    hosted_override: Option<bool>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
            #[cfg(test)]
            hosted_override: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<ImageEditor>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        #[cfg(test)]
        if let Some(hosted) = self.hosted_override {
            return hosted;
        }
        self.framework.is_some()
    }

    /// Mark this surface as framework-hosted so
    /// [`PublishesResources`](ui::resources::PublishesResources) keeps only
    /// live extras beside the published project door.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_hosted_for_test(&mut self, hosted: bool) {
        self.hosted_override = Some(hosted);
    }

    pub(crate) fn dispatch_with_context<A>(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &A,
        context: ui::tilo_actions::ActionContext,
    ) where
        A: tilo_actions::Action + serde::Serialize + crate::actions::AsEditorAction,
    {
        match &self.framework {
            Some(dispatcher) => {
                let editor = action.as_editor_action();
                self.last =
                    tilo_actions::EncodedAction::encode(IMAGE_EDITOR_ACTION_TYPE, &editor).ok();
                let _ = dispatcher.apply_now_with(doc, &editor, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
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

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<ImageEditorShell>()
}

pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            "--open" | "--size" => {
                let _ = args.next();
                return true;
            }
            flag if flag.starts_with("--shots=") || flag.starts_with("--open=") => return true,
            file if !file.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ADD_ADJUSTMENT_TYPE, PROJECT_RESOURCE_ID};
    use crate::model::{Operation, Project};
    use std::rc::Rc;
    use ui::framework::ResourceSurface;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn publish_includes_add_adjustment_document_ops() {
        let project = Project::blank();
        let published = ui::tilo_resource::publish::<ImageEditor>(
            PROJECT_RESOURCE_ID,
            "Image Editor",
            &project,
        );
        assert_eq!(published.kind, ImageEditor::KIND);
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "exposure",
            "contrast",
            "colour",
            "mono",
            "rotate",
            "crop",
            "open",
            ui::tilo_resource::APPLY,
            ui::tilo_resource::GROUP,
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish via bound_actions/local_operations: {ids:?}"
            );
        }
        for id in ["exposure", "contrast", "colour", "mono", "rotate", "crop"] {
            let add = published
                .actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing add-{id}"));
            assert_eq!(
                add.kind,
                OperationKind::Document,
                "`{id}` is a Document graph edit"
            );
            assert_eq!(
                add.operation_type.as_deref(),
                None,
                "immediate Document specs carry no operation_type"
            );
            let encoded = add
                .build_json(&serde_json::Map::new())
                .expect("add-adjustment builds without args");
            assert_eq!(encoded.type_id, ADD_ADJUSTMENT_TYPE);
        }
        assert!(
            ids.contains(&"open"),
            "open Effect rides local_operations: {ids:?}"
        );
    }

    #[test]
    fn hosted_resources_do_not_republish_project_root() {
        let mut project = Project::blank();
        assert!(project.insert_before_output(
            "exposure-1",
            "Exposure",
            Operation::Exposure { stops: 0.5 },
        ));
        let published = ui::tilo_resource::publish::<ImageEditor>(
            PROJECT_RESOURCE_ID,
            "Image Editor",
            &project,
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.as_str()
                    == crate::actions::adjustment_resource_id("exposure-1")),
            "publish children carry adjustment Document ops: {:?}",
            published
                .children
                .iter()
                .map(|child| child.id.as_str())
                .collect::<Vec<_>>()
        );
        let adjustment = published
            .children
            .iter()
            .find(|child| child.id.as_str() == crate::actions::adjustment_resource_id("exposure-1"))
            .expect("adjustment child");
        for expected in ["select", "set-stops", "move", "remove"] {
            assert!(
                adjustment
                    .actions
                    .iter()
                    .any(|action| action.id == expected),
                "adjustment must publish `{expected}`: {:?}",
                adjustment
                    .actions
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
            );
        }

        let mut app = ImageEditorApp::new();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            !app.is_framework_hosted(),
            "standalone stays on the full action tree"
        );
        app.seed_unpublished_project_for_test(project.clone());

        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, ImageEditor::KIND);
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "exposure"),
            "standalone publish includes add-exposure"
        );

        app.set_framework_hosted(true);
        assert!(app.is_framework_hosted());
        assert!(ui::resources::resource(&app, &state.doc, PROJECT_RESOURCE_ID).is_none());
        assert!(
            ui::resources::resource(
                &app,
                &state.doc,
                &crate::actions::adjustment_resource_id("exposure-1"),
            )
            .is_some(),
            "a hosted adjustment stays the published row"
        );
        let extras = app.resources(&state.doc);
        assert!(
            extras.is_empty(),
            "hosted returns no second image root (crop/export are widget-local): {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != ImageEditor::KIND
                    && resource.id.as_str() != PROJECT_RESOURCE_ID),
            "hosted must not republish the project beside ResourceRoot"
        );
    }

    #[test]
    fn present_blank_does_not_wipe_unpublished_local_edits() {
        let mut app = ImageEditorApp::new();
        let mut local = Project::blank();
        assert!(local.insert_before_output(
            "exposure-local",
            "Exposure",
            Operation::Exposure { stops: 0.75 },
        ));
        if let Some(Operation::Source { path }) = local.operations.get_mut("source") {
            *path = "/tmp/local-only.png".into();
        }
        app.seed_unpublished_project_for_test(local);
        assert_eq!(
            app.local_adjustment_ids_for_test(),
            vec!["exposure-local".to_owned()]
        );
        assert_eq!(app.local_source_path_for_test(), "/tmp/local-only.png");

        let blank = Rc::new(Project::blank());
        ResourceSurface::<ImageEditor>::present(&mut app, &blank);
        assert_eq!(
            app.local_adjustment_ids_for_test(),
            vec!["exposure-local".to_owned()],
            "blank present must not wipe an unpublished local edit stack"
        );
        assert_eq!(
            app.local_source_path_for_test(),
            "/tmp/local-only.png",
            "blank present must keep an unpublished local source path"
        );
    }
}
