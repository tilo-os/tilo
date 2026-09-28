//! Feeds as a [`tilo_resource::Resource`]: subscriptions are document state;
//! refresh/import are Effects in [`Resource::local_operations`].

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    FEEDS_ACTION_TYPE, FeedsAction, FeedsResourceCtx, provide_resources, register,
};
use crate::app::FeedsApp;
use crate::doc::FeedsDoc;
use crate::model::Store;

/// The reader store projection.
pub type FeedsState = Store;

/// The feeds reader resource.
pub struct Feeds;

impl Resource for Feeds {
    const KIND: &'static str = "feeds.reader";
    const LABEL: &'static str = "Feeds";
    const ACTION_TYPE: &'static str = FEEDS_ACTION_TYPE;

    type State = FeedsState;
    type Action = FeedsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<FeedsState, ModelError> {
        Ok(FeedsDoc::load(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted new-folder / add-subscription stay on the published tree.
        Vec::new()
    }

    fn local_operations() -> Vec<ActionSpec> {
        provide_resources(&FeedsResourceCtx::default(), &AutoCommit::new())
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &FeedsState) -> Vec<SearchEntry> {
        let feeds = state.feeds.iter().map(|feed| SearchEntry {
            anchor: Some(format!("feeds.subscription:{}", feed.id)),
            title: feed.title.clone(),
            text: feed.url.clone(),
        });
        let articles = state.articles.iter().map(|article| SearchEntry {
            anchor: Some(format!("feeds.article:{}", article.id)),
            title: article.title.clone(),
            text: article.summary.clone(),
        });
        feeds.chain(articles).collect()
    }
}

/// Framework app.
pub struct FeedsShell;

impl ResourceApp for FeedsShell {
    type Resource = Feeds;
    const ID: &'static str = "so.tilo.feeds";
    const TITLE: &'static str = "Feeds";
    const ICON: Option<&'static str> = Some("feeds");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Feeds>) -> Element {
        cx.surface(FeedsApp::new())
    }
}

impl ResourceSurface<Feeds> for FeedsApp {
    fn present(&mut self, state: &std::rc::Rc<FeedsState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Feeds>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Feeds>>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Feeds>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &FeedsAction,
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

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }
}
