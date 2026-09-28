//! Snippets as a [`tilo_resource::Resource`]: the library syncs; search focus
//! and reveal are Views.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    SNIPPETS_ACTION_TYPE, SnippetsAction, SnippetsResourceCtx, provide_contextual_resources,
    provide_resources, register, snippet_resource_id,
};
use crate::app::SnippetsApp;
use crate::doc::SnippetsDoc;
use crate::model::Snippet;

/// The snippets library resource.
pub struct Snippets;

impl Resource for Snippets {
    const KIND: &'static str = "snippets.library";
    const LABEL: &'static str = "Snippets";
    const ACTION_TYPE: &'static str = SNIPPETS_ACTION_TYPE;

    type State = Vec<Snippet>;
    type Action = SnippetsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Vec<Snippet>, ModelError> {
        Ok(SnippetsDoc::list(doc))
    }

    fn local_operations() -> Vec<ActionSpec> {
        provide_contextual_resources(&SnippetsResourceCtx::default())
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn children(state: &Vec<Snippet>) -> Vec<tilo_actions::Resource> {
        let mut doc = ui::automerge::AutoCommit::new();
        for snippet in state {
            SnippetsDoc::upsert(&mut doc, snippet);
        }
        provide_resources(&SnippetsResourceCtx::default(), &doc)
            .into_iter()
            .flat_map(|root| root.children)
            .collect()
    }

    fn search(state: &Vec<Snippet>) -> Vec<SearchEntry> {
        state
            .iter()
            .map(|snippet| SearchEntry {
                anchor: Some(snippet_resource_id(snippet.id)),
                title: if snippet.title.trim().is_empty() {
                    "Untitled snippet".into()
                } else {
                    snippet.title.clone()
                },
                text: snippet.description.clone(),
            })
            .collect()
    }
}

/// Framework app: the snippets widget over [`Snippets`].
pub struct SnippetsShell;

impl ResourceApp for SnippetsShell {
    type Resource = Snippets;
    const ID: &'static str = "so.tilo.snippets";
    const TITLE: &'static str = "Snippets";
    const ICON: Option<&'static str> = Some("snippets");
    const SHOT_VIEWPORT: (u32, u32) = (1100, 720);

    fn view(cx: &mut ViewCx<Snippets>) -> Element {
        cx.surface(SnippetsApp::new())
    }
}

impl ResourceSurface<Snippets> for SnippetsApp {
    fn present(&mut self, state: &std::rc::Rc<Vec<Snippet>>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Snippets>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Snippets>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Snippets>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &SnippetsAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
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

/// Live launch through the framework host, with History mode registered.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<SnippetsShell>().history(&crate::framework_history::HISTORY)
}
