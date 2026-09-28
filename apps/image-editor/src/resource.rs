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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<ImageEditor>) {
        self.framework = Some(dispatcher);
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
