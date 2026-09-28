//! Code Editor as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path. Buffer text lives in the shared document as collaborative
//! file records under the project root.

use std::rc::Rc;

use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{CODE_EDITOR_ACTION_TYPE, CodeEditorAction, register};
use crate::app::CodeApp;
use crate::doc::{CollaborativeFile, EditorDoc};

/// Lightweight project projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct ProjectState {
    pub project_id: Option<String>,
    pub files: Vec<CollaborativeFile>,
}

/// The code-editor project resource.
pub struct CodeEditor;

impl Resource for CodeEditor {
    const KIND: &'static str = "code_editor.project";
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
        let files = EditorDoc::files(doc)
            .into_iter()
            .filter(|file| !file.deleted)
            .collect();
        Ok(ProjectState { project_id, files })
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // Live editor starts empty — no demo tree and no sample files.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted new-file / rename specs stay on the published tree.
        Vec::new()
    }

    fn children(state: &ProjectState) -> Vec<tilo_actions::Resource> {
        state
            .files
            .iter()
            .filter(|file| file.kind == "file")
            .map(|file| {
                tilo_actions::Resource::new(
                    format!("code_editor.file:{}", file.id),
                    "code_editor.file",
                    file.name.clone(),
                )
            })
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
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<CodeEditor>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher for file records and text edits.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<CodeEditor>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<CodeEditor>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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
