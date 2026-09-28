//! Git client as a [`tilo_resource::Resource`]: collaborative settings sync;
//! fetch/pull/push and working-tree ops are Effects against the local repo.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{GIT_CLIENT_ACTION_TYPE, GitClientAction, register};
use crate::app::GitClientApp;
use crate::model::RepoModel;
use crate::settings::Settings;

/// The git client resource. Document state is preference settings; the open
/// repository itself is a local Effect target (`git_client.repo` kind).
pub struct GitClient;

impl Resource for GitClient {
    const KIND: &'static str = "git_client.repo";
    const LABEL: &'static str = "Repository";
    const ACTION_TYPE: &'static str = GIT_CLIENT_ACTION_TYPE;

    type State = Settings;
    type Action = GitClientAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<Settings, ModelError> {
        Ok(Settings::read_only(doc))
    }

    fn local_operations() -> Vec<ActionSpec> {
        // View/Effect catalogue for an empty repository projection. Concrete
        // fetch/pull/push Effects (and working-tree ops) attach when a repo is
        // open; there is no outbox — network ops are not undone via document
        // inverse today.
        let model = RepoModel::default();
        crate::actions::provide_resources(&model, &ui::automerge::AutoCommit::new())
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app: the git client widget over [`GitClient`].
pub struct GitClientShell;

impl ResourceApp for GitClientShell {
    type Resource = GitClient;
    const ID: &'static str = "so.tilo.git-client";
    const TITLE: &'static str = "Git";
    const ICON: Option<&'static str> = Some("git-client");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<GitClient>) -> Element {
        cx.surface(GitClientApp::new())
    }
}

impl ResourceSurface<GitClient> for GitClientApp {
    fn present(&mut self, state: &std::rc::Rc<Settings>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<GitClient>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<GitClient>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<GitClient>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &GitClientAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = crate::actions::encode(action).ok();
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

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<GitClientShell>().configure_app(|app| {
        app.set_window_title(Some("Git Client".into()));
    })
}
