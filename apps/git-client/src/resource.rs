//! Git client as a [`tilo_resource::Resource`]: preferences and the shared
//! repository projection sync; working-tree status and in-flight remotes stay
//! live extras when hosted.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    GIT_CLIENT_ACTION_TYPE, GitClientAction, document_bound_actions, publish_children, register,
    root_local_operations,
};
use crate::app::GitClientApp;
use crate::doc::GitClientState;

/// The git client resource. Document state is preferences plus the shared
/// repository projection; working-tree rows and in-flight fetch/push are live.
pub struct GitClient;

impl Resource for GitClient {
    const KIND: &'static str = "git_client.repo";
    const LABEL: &'static str = "Repository";
    const ACTION_TYPE: &'static str = GIT_CLIENT_ACTION_TYPE;

    type State = GitClientState;
    type Action = GitClientAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<GitClientState, ModelError> {
        Ok(GitClientState::from_doc(doc))
    }

    fn bound_actions(state: &GitClientState) -> Vec<ActionSpec> {
        document_bound_actions(&state.settings)
    }

    fn children(state: &GitClientState) -> Vec<PublishedResource> {
        publish_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // View navigation and working-tree / branch / stash Effects. Per-remote
        // fetch/pull/push ride remote children from [`children`].
        root_local_operations()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Hosted-only extras that are not document state: uncommitted working-tree
/// status and an in-flight remote job. The repository door stays on
/// [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &GitClientApp) -> Vec<PublishedResource> {
    let mut extras = Vec::new();
    if let Some(model) = app.resource_model() {
        extras.push(crate::actions::changes_collection(model));
    }
    if let Some(label) = app.remote_job_label() {
        extras.push(PublishedResource::new(
            "git_client.remote-job",
            "git_client.job",
            label,
        ));
    }
    extras
}

/// Framework app: the git client widget over [`GitClient`].
pub struct GitClientShell;

impl ResourceApp for GitClientShell {
    type Resource = GitClient;
    const ID: &'static str = "so.tilo.git-client";
    const TITLE: &'static str = "Git";
    const ICON: Option<&'static str> = Some("git-client");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn label(state: &GitClientState) -> String {
        crate::actions::repo_label(&state.repo)
    }

    fn view(cx: &mut ViewCx<GitClient>) -> Element {
        cx.surface(GitClientApp::new())
    }
}

impl ResourceSurface<GitClient> for GitClientApp {
    fn present(&mut self, state: &std::rc::Rc<GitClientState>) {
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
    hosted: bool,
    last: Option<EncodedAction>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<GitClient>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::provide_resources;
    use crate::doc::{SharedRepo, shared_fingerprint};
    use crate::settings::Settings;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_repo_door_and_hosted_keeps_only_live_extras() {
        let (model, _) = crate::sample::model();
        let mut doc = AutoCommit::new();
        Settings::default().write(&mut doc);
        SharedRepo::write(&mut doc, &SharedRepo::from_model(&model));
        doc.commit();

        let state = GitClient::read(&doc).expect("seeded git state");
        let published =
            ui::tilo_resource::publish::<GitClient>("git_client.repo", "renderer", &state);
        assert_eq!(published.kind, "git_client.repo");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "diff-layout"),
            "preference bound_actions must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "show-changes"),
            "show-changes View must be on the framework resource"
        );
        assert!(
            published.actions.iter().any(|action| action.id == "commit"),
            "commit Effect must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.as_str() == "git_client.branches"),
            "branches child must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.as_str() == "git_client.remotes"),
            "remotes child must be on the framework resource"
        );
        assert!(
            published
                .children
                .iter()
                .all(|child| child.id.as_str() != "git_client.changes"),
            "shared projection must not carry working-tree Changes"
        );
        let remote = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "git_client.remotes")
            .and_then(|collection| collection.children.first())
            .expect("sample remote");
        assert!(
            remote.actions.iter().any(|action| action.id == "fetch"),
            "fetch Effect must ride the remote child"
        );

        let mut app = GitClientApp::from_model(model.clone(), Default::default());
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "git_client.repo");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == "git_client.changes"),
            "standalone publishes working-tree Changes from the live model"
        );
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == "git_client.branches"),
            "standalone publishes the shared tree"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "git_client.repo").is_none());
        let hosted_branches = ui::resources::resource(&app, &seeded, "git_client.branches")
            .expect("a hosted branch catalogue is the published row");
        assert!(
            hosted_branches
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .eq(published
                    .children
                    .iter()
                    .find(|child| child.id.as_str() == "git_client.branches")
                    .expect("published branches")
                    .actions
                    .iter()
                    .map(|action| action.id.as_str()))
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "git_client.repo"),
            "hosted must not republish git_client.repo beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.id.as_str() == "git_client.changes"),
            "hosted still publishes working-tree Changes: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&model, &seeded);
        assert_eq!(door[0].kind, "git_client.repo");
        assert_eq!(
            shared_fingerprint(&model),
            shared_fingerprint(&SharedRepo::from_model(&model).into_model()),
            "shared fingerprint ignores working-tree rows"
        );
    }
}
