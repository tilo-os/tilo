//! Code Editor as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path. Buffer text lives in the shared document as collaborative
//! file records under the project root.
//!
//! Document mutations (create / rename / move / language / delete / replace-text)
//! live on this resource via [`Resource::declarations`] / [`Resource::children`].
//! Commands and open-file/folder Effects are [`Resource::local_operations`].
//! Hosted [`PublishesResources::resources`] returns only live extras (save,
//! search hits, symbols) — never a second hand-built project Document door
//! beside [`tilo_resource::publish`].

use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionParam, ActionSpec, EncodedAction, OperationKind, PROMPTED_PARAM, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    CODE_EDITOR_ACTION_TYPE, CodeEditorAction, create_file_action, live_presentation_extras,
    project_view_effect_specs, publish_document_children, register,
};
use crate::app::CodeApp;
use crate::doc::{CollaborativeFile, EditorDoc};

/// Stable id prefix for the published project root.
pub const PROJECT_KIND: &str = "code_editor.project";

/// Lightweight project projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct ProjectState {
    pub project_id: Option<String>,
    pub files: Vec<CollaborativeFile>,
    /// Buffer text keyed by file id — base for replace-text Document ops.
    pub buffers: HashMap<String, String>,
}

/// The code-editor project resource.
pub struct CodeEditor;

impl Resource for CodeEditor {
    const KIND: &'static str = PROJECT_KIND;
    const LABEL: &'static str = "Code Editor";
    const ACTION_TYPE: &'static str = CODE_EDITOR_ACTION_TYPE;

    type State = ProjectState;
    type Action = CodeEditorAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<ProjectState, ModelError> {
        let project_id = match doc.get(ROOT, "project_id") {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(id) => Some(id.to_string()),
                _ => None,
            },
            _ => None,
        };
        let files: Vec<_> = EditorDoc::files(doc)
            .into_iter()
            .filter(|file| !file.deleted)
            .collect();
        let mut buffers = HashMap::new();
        for file in files.iter().filter(|file| file.kind == "file") {
            if let Some(text) = EditorDoc::read_buffer_key(doc, &file.id) {
                buffers.insert(file.id.clone(), text);
            }
        }
        Ok(ProjectState {
            project_id,
            files,
            buffers,
        })
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // Live editor starts empty — no demo tree and no sample files.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-file",
                "New file",
                vec![ActionParam::optional(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "File name",
                )],
                |args, _| {
                    let name = args
                        .text(PROMPTED_PARAM)
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or("untitled")
                        .to_string();
                    Ok(create_file_action(
                        uuid::Uuid::new_v4().to_string(),
                        name,
                        None,
                        i64::MAX / 2,
                        String::new(),
                    ))
                },
            )
            .describe("Create a new collaborative file at the project root."),
            ActionDecl::new(
                "new-folder",
                "New folder",
                vec![ActionParam::optional(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Folder name",
                )],
                |args, _| {
                    let name = args
                        .text(PROMPTED_PARAM)
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or("untitled")
                        .to_string();
                    Ok(CodeEditorAction::CreateFolder {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        parent: None,
                        order: i64::MAX / 2,
                    })
                },
            )
            .describe("Create a new folder at the project root."),
        ]
    }

    fn children(state: &ProjectState) -> Vec<PublishedResource> {
        publish_document_children(&state.files, &state.buffers)
    }

    fn local_operations() -> Vec<ActionSpec> {
        project_view_effect_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &ProjectState) -> Vec<SearchEntry> {
        state
            .files
            .iter()
            .map(|file| SearchEntry {
                anchor: Some(format!("code_editor.{}:{}", file.kind, file.id)),
                title: file.name.clone(),
                text: file.path.clone(),
            })
            .collect()
    }
}

/// Hosted-only extras that are not document state: save Effects, symbols, and
/// search-result Views. The project Document door stays on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &CodeApp) -> Vec<PublishedResource> {
    live_presentation_extras(&app.code_resource_ctx())
}

/// Framework app: the code editor widget is the surface over [`CodeEditor`].
pub struct CodeEditorShell;

impl ResourceApp for CodeEditorShell {
    type Resource = CodeEditor;
    const ID: &'static str = "so.tilo.code-editor";
    const TITLE: &'static str = "Code Editor";
    const ICON: Option<&'static str> = Some("code-editor");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<CodeEditor>) -> Element {
        cx.surface(CodeApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "terminal",
            "selection",
            "completion",
            "find",
            "search",
            "conflict",
            "indexing",
            "goto",
            "close-prompt",
            "status",
        ]
    }
}

impl ResourceSurface<CodeEditor> for CodeApp {
    fn present(&mut self, state: &Rc<ProjectState>) {
        // Adopt shared file records only — never wipe an unpublished local tree.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<CodeEditor>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for file records and text edits.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<CodeEditor>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<CodeEditor>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &CodeEditorAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        let _ = self.try_dispatch_with_context(doc, type_id, action, context);
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &CodeEditorAction,
        context: ui::tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = crate::actions::encode(action).ok();
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
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

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{create_file_action, file_resource_id};
    use tilo_actions::Action;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_file_document_ops_and_hosted_keeps_no_second_door() {
        let mut doc = AutoCommit::new();
        let file_id = uuid::Uuid::new_v4().to_string();
        create_file_action(
            file_id.clone(),
            "door.rs".into(),
            None,
            0,
            "fn main() {}".into(),
        )
        .apply(&mut doc)
        .expect("seed file");
        doc.commit();

        let state = CodeEditor::read(&doc).expect("seeded project state");
        let project_id = state
            .project_id
            .clone()
            .unwrap_or_else(|| "test-project".into());
        let published = ui::tilo_resource::publish::<CodeEditor>(
            &crate::actions::project_resource_id(&project_id),
            "Code Editor",
            &state,
        );
        assert_eq!(published.kind, PROJECT_KIND);
        for id in ["new-file", "new-folder", "open-file", "open-folder"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework project resource"
            );
        }
        let file = published
            .children
            .iter()
            .find(|child| child.id.as_str() == file_resource_id(&file_id))
            .expect("file child on the published door");
        for id in ["rename", "move", "set-language", "delete", "replace-text"] {
            assert!(
                file.actions
                    .iter()
                    .any(|action| action.id == id && action.kind == OperationKind::Document),
                "{id} Document op must ride the file child"
            );
        }

        let mut app = CodeApp::new();
        let state_for_app = CodeEditor::read(&doc).expect("seeded project state");
        app.adopt_resource_state(&state_for_app);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&doc);
        assert!(
            standalone
                .iter()
                .any(|resource| resource.kind == PROJECT_KIND),
            "standalone keeps the project root"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let root_id = standalone[0].id.as_str().to_owned();
        assert!(ui::resources::resource(&app, &doc, &root_id).is_none());
        let hosted_file =
            ui::resources::resource(&app, &doc, &file_resource_id(&file_id)).expect("hosted file");
        assert!(
            hosted_file
                .actions
                .iter()
                .any(|action| action.id == "rename" && action.kind == OperationKind::Document),
            "a hosted file is the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &doc);
        assert!(
            extras.iter().all(|resource| resource.kind != PROJECT_KIND),
            "hosted must not republish code_editor.project beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let directory =
            ui::resources::PublishesResources::action_directory_resource_page(&app, &doc, &[], 8)
                .expect("hosted directory");
        assert!(
            directory
                .resources
                .iter()
                .all(|resource| resource.kind != PROJECT_KIND),
            "hosted directory must not republish the project: {:?}",
            directory
                .resources
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );

        // Empty present must not wipe unpublished local files.
        let mut local = CodeApp::new();
        local.adopt_resource_state(&state_for_app);
        let before = local.project_node_count();
        assert!(before > 0, "seeded app has a local tree");
        local.adopt_resource_state(&ProjectState::default());
        assert_eq!(
            local.project_node_count(),
            before,
            "adopting an empty projection must not clear unpublished files"
        );
    }
}
