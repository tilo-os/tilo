//! Snippets as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for the library.
//!
//! Document mutations (`new-snippet` under All Snippets, per-snippet rename /
//! language / tags / star / delete, …) live on this resource via
//! [`Resource::declarations`] / [`Resource::children`]. Search focus is a View
//! ([`Resource::local_operations`]). Hosted [`PublishesResources::resources`]
//! returns only the peer's sidebar-folder create (`snippets.folder`) — never a
//! second hand-built `snippets.library` root beside [`tilo_resource::publish`].

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionParam, ActionSpec, EncodedAction, OperationKind, PROMPTED_PARAM, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    SNIPPETS_ACTION_TYPE, SnippetsAction, create_snippet_action, library_view_specs,
    publish_children, register, snippet_resource_id,
};
use crate::app::SnippetsApp;
use crate::doc::SnippetsDoc;
use crate::model::Snippet;

/// Stable id for the published library root.
pub const LIBRARY_RESOURCE_ID: &str = "snippets.library";

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

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-snippet",
                "New snippet",
                vec![ActionParam::required(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Title",
                )],
                |args, _| {
                    let title = args.text(PROMPTED_PARAM).unwrap_or_default();
                    let mut snippet = Snippet::blank("");
                    snippet.id = Uuid::new_v4();
                    let trimmed = title.trim();
                    if !trimmed.is_empty() {
                        snippet.title = trimmed.to_string();
                    }
                    Ok(create_snippet_action(snippet))
                },
            )
            .describe("Create a new snippet from a typed title."),
        ]
    }

    fn children(state: &Vec<Snippet>) -> Vec<PublishedResource> {
        publish_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        library_view_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
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
        // Adopt the shared library projection only — never wipe a local
        // library that has not been published yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Snippets>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Snippets>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Snippets>) {
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

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &Vec<Snippet>) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Snippets>(
        LIBRARY_RESOURCE_ID,
        "Snippets",
        state,
    )]
}

/// Live launch through the framework host, with History mode registered.
#[cfg(not(target_arch = "wasm32"))]
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<SnippetsShell>().history(&crate::framework_history::HISTORY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::Action;
    use ui::widget::Widget;

    #[test]
    fn resource_publish_owns_library_door_and_hosted_keeps_folder_extras() {
        let mut doc = AutoCommit::new();
        let mut snippet = Snippet::blank("Ops");
        snippet.title = "Deploy".into();
        snippet.tags = vec!["k8s".into()];
        create_snippet_action(snippet.clone())
            .apply(&mut doc)
            .unwrap();
        doc.commit();

        let state = Snippets::read(&doc).expect("seeded snippets state");
        let published =
            ui::tilo_resource::publish::<Snippets>(LIBRARY_RESOURCE_ID, "Snippets", &state);
        assert_eq!(published.kind, "snippets.library");
        for id in ["new-snippet", "search"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "snippets.tag"
                    && child.actions.iter().any(|action| action.id == "reveal")),
            "tag children keep their reveal View"
        );
        let snippet_child = published
            .children
            .iter()
            .find(|child| child.kind == "snippets.snippet")
            .expect("snippet child on the published door");
        for id in [
            "open",
            "edit-body",
            "rename",
            "set-language",
            "move-to-folder",
            "set-tags",
            "edit-description",
            "toggle-favorite",
            "delete",
        ] {
            assert!(
                snippet_child.actions.iter().any(|action| action.id == id),
                "{id} must be on the published snippet child"
            );
        }

        let mut app = SnippetsApp::new_demo();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "snippets.library");
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "new-snippet"),
            "standalone library keeps New snippet"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let without_folder = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            without_folder.is_empty(),
            "All Snippets has no folder extra beyond publish declarations: {:?}",
            without_folder
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>()
        );

        app.select_folder_for_test("Ops");
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "snippets.library"),
            "hosted must not republish snippets.library beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(
            extras.len(),
            1,
            "hosted returns the sidebar-folder slice alone"
        );
        assert_eq!(extras[0].kind, "snippets.folder");
        assert_eq!(extras[0].id.as_str(), actions::folder_resource_id("Ops"));
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id == "new-snippet"),
            "hosted keeps folder-bound New snippet"
        );
        assert!(
            !extras[0].actions.iter().any(|action| action.id == "search"
                || action.id == "rename"
                || action.id == "delete"),
            "hosted folder must not copy library Views or per-snippet Document ops already on publish"
        );
        assert!(
            extras[0].children.is_empty(),
            "hosted folder must not nest per-snippet children already on publish"
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "snippets.library");
        assert!(
            door[0]
                .actions
                .iter()
                .any(|action| action.id == "new-snippet")
        );
        assert!(
            door[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == actions::snippet_resource_id(snippet.id)),
            "publish children carry per-snippet document ops"
        );

        // Empty present must not wipe a local library that has not been published.
        let mut local = SnippetsApp::new_demo();
        let mut local_doc = AutoCommit::new();
        local.sync(&mut local_doc, &ui::automerge::ROOT);
        let before_local = local.snippet_count();
        assert!(before_local > 0, "demo app starts with a local library");
        local.adopt_resource_state(&[]);
        assert_eq!(
            local.snippet_count(),
            before_local,
            "adopting an empty projection must not clear unpublished snippets"
        );
    }
}
