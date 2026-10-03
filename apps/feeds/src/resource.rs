//! Feeds as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the library.
//!
//! Document mutations (`new-folder`, `add-subscription`, per-folder / per-feed /
//! per-article ops) live on this resource via [`Resource::declarations`] /
//! [`Resource::bound_actions`] / [`Resource::children`]. OPML import/export and
//! refresh are Effects / Views in [`Resource::local_operations`]. Hosted
//! [`PublishesResources::resources`] stays empty — never a second hand-built
//! `feeds.reader` root beside [`tilo_resource::publish`].

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
    FEEDS_ACTION_TYPE, FeedsAction, add_folder_action, add_subscription_action,
    bound_reader_actions, publish_children, reader_effect_specs, register,
    resource_ctx_from_library,
};
use crate::app::FeedsApp;
use crate::doc::FeedsDoc;
use crate::model::{ReadSettings, Store};

/// Stable id for the published reader root.
pub const READER_RESOURCE_ID: &str = "feeds.reader";

/// Shared library projection: subscriptions plus durable reading settings.
#[derive(Clone, Debug, Default)]
pub struct FeedsState {
    pub store: Store,
    pub settings: ReadSettings,
}

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
        Ok(FeedsState {
            store: FeedsDoc::load(doc),
            settings: FeedsDoc::read_settings(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Prompted creates also appear as bound ActionSpecs with live next-order
        // values; keeping named declarations here makes the door discoverable
        // without a full library snapshot (agents / APPLY path).
        vec![
            ActionDecl::new(
                "add-subscription",
                "Add subscription",
                vec![ActionParam::required(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Feed URL",
                )],
                |args, _| {
                    let url = args.text(PROMPTED_PARAM).unwrap_or_default();
                    Ok(add_subscription_action(Uuid::new_v4(), url, None, 0))
                },
            )
            .describe("Subscribe to a feed URL."),
            ActionDecl::new(
                "new-folder",
                "New folder",
                vec![ActionParam::required(
                    PROMPTED_PARAM,
                    ParamType::Text,
                    "Name",
                )],
                |args, _| {
                    let name = args.text(PROMPTED_PARAM).unwrap_or_default();
                    Ok(add_folder_action(Uuid::new_v4(), name, 0))
                },
            )
            .describe("Create a subscription folder."),
        ]
    }

    fn bound_actions(state: &FeedsState) -> Vec<ActionSpec> {
        // Live next-order / unread payloads. Nested folder/feed/article Document
        // ops ride [`Self::children`].
        bound_reader_actions(&resource_ctx_from_library(&state.store, state.settings))
            .into_iter()
            .filter(|spec| {
                // declarations already publish add-subscription / new-folder by
                // name; keep mark-all-read here (needs unread ids from state).
                spec.id == "mark-all-read"
            })
            .collect()
    }

    fn children(state: &FeedsState) -> Vec<PublishedResource> {
        publish_children(&resource_ctx_from_library(&state.store, state.settings))
    }

    fn local_operations() -> Vec<ActionSpec> {
        // No live subscription count here — host refresh still refuses an empty
        // library at invoke time. Standalone provide_resources binds availability.
        reader_effect_specs(true)
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &FeedsState) -> Vec<SearchEntry> {
        let feeds = state.store.feeds.iter().map(|feed| SearchEntry {
            anchor: Some(format!("feeds.subscription:{}", feed.id)),
            title: feed.title.clone(),
            text: feed.url.clone(),
        });
        let articles = state.store.articles.iter().map(|article| SearchEntry {
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
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Feeds>) -> Element {
        cx.surface(FeedsApp::new())
    }
}

impl ResourceSurface<Feeds> for FeedsApp {
    fn present(&mut self, state: &std::rc::Rc<FeedsState>) {
        // Adopt the shared library projection only — never wipe a local library
        // that has not been published yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Feeds>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Feeds>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Feeds>) {
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
        action: &FeedsAction,
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

    #[allow(
        dead_code,
        reason = "parity with sibling ActionSinks; useful when asserting hosted dispatch"
    )]
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
pub fn provide_resources(state: &FeedsState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Feeds>(
        READER_RESOURCE_ID,
        "Feeds",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::Action;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one assertion covers publish ownership and hosted extras"
    )]
    fn resource_publish_owns_library_door_and_hosted_keeps_no_second_tree() {
        let mut doc = AutoCommit::new();
        let folder_id = Uuid::from_u128(0xFEED_0001);
        let feed_id = Uuid::from_u128(0xFEED_0002);
        let article_id = Uuid::from_u128(0xFEED_0003);
        add_folder_action(folder_id, "News", 0)
            .apply(&mut doc)
            .unwrap();
        add_subscription_action(feed_id, "https://example.test/feed.xml", Some(folder_id), 0)
            .apply(&mut doc)
            .unwrap();
        FeedsAction::UpsertFetched {
            feeds: vec![crate::model::Feed {
                id: feed_id,
                title: "Example".into(),
                url: "https://example.test/feed.xml".into(),
                site_url: "https://example.test/".into(),
                folder: Some(folder_id),
                order: 0,
                kind: crate::model::FeedKind::Rss,
            }],
            articles: vec![crate::model::Article {
                id: article_id,
                feed_id,
                title: "Hello".into(),
                url: "https://example.test/hello".into(),
                summary: "hi".into(),
                author: String::new(),
                published: 0,
                read: false,
                starred: false,
                tags: Vec::new(),
                blocks: Vec::new(),
            }],
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();

        let state = Feeds::read(&doc).expect("seeded feeds state");
        let published = ui::tilo_resource::publish::<Feeds>(READER_RESOURCE_ID, "Feeds", &state);
        assert_eq!(published.kind, "feeds.reader");
        for id in [
            "add-subscription",
            "new-folder",
            "mark-all-read",
            "import-opml",
            "export-opml",
            "refresh-all",
        ] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "feeds.folder"
                    && child.actions.iter().any(|action| action.id == "rename")),
            "folder children keep rename Document ops"
        );
        let feed_child = published
            .children
            .iter()
            .find(|child| child.kind == "feeds.subscription")
            .expect("subscription child on the published door");
        for id in ["open", "rename", "mark-read", "move", "unsubscribe"] {
            assert!(
                feed_child.actions.iter().any(|action| action.id == id),
                "{id} must be on the published subscription child"
            );
        }
        let article_child = feed_child
            .children
            .iter()
            .find(|child| child.kind == "feeds.article")
            .expect("article nested under subscription");
        for id in ["open", "read", "star", "set-tags"] {
            assert!(
                article_child.actions.iter().any(|action| action.id == id),
                "{id} must be on the published article child"
            );
        }

        let mut app = FeedsApp::new_demo();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "feeds.reader");
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "new-folder"),
            "standalone reader keeps New folder"
        );
        assert!(
            standalone[0]
                .actions
                .iter()
                .any(|action| action.id == "add-subscription"),
            "standalone reader keeps Add subscription"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras.is_empty(),
            "hosted must not republish feeds.reader beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "feeds.reader");
        assert!(
            door[0]
                .actions
                .iter()
                .any(|action| action.id == "new-folder")
        );
        assert!(
            door[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == crate::actions::feed_resource_id(feed_id)),
            "publish children carry per-feed document ops"
        );

        // Empty present must not wipe a local library that has not been published.
        let mut local = FeedsApp::new_demo();
        let mut local_doc = AutoCommit::new();
        local.sync(&mut local_doc, &ui::automerge::ROOT);
        let before = local.store_feed_ids().len();
        assert!(before > 0, "demo app starts with a local library");
        local.adopt_resource_state(&FeedsState::default());
        assert_eq!(
            local.store_feed_ids().len(),
            before,
            "adopting an empty projection must not clear unpublished feeds"
        );
    }

    /// What a refresh fetched reaches every collaborator but is nobody's
    /// undo step, while an OPML import is one step.
    #[test]
    fn fetched_rows_are_no_undo_step_and_an_import_is_one() {
        use crate::actions::{add_subscription_action, rename_feed_action};
        use crate::model::{Article, Feed, FeedKind, Folder};
        use ui::framework::testing::TestApp;
        use ui::tilo_resource::Invoke;

        let apply = |action: &FeedsAction| Invoke::<Feeds>::apply(action).unwrap();
        let mut app = TestApp::<FeedsShell>::new().unwrap();
        let feed_id = Uuid::from_u128(0xFEED_0010);
        app.invoke(apply(&add_subscription_action(
            feed_id,
            "https://example.test/feed.xml",
            None,
            0,
        )))
        .unwrap();
        app.invoke(apply(&rename_feed_action(feed_id, "Mine")))
            .unwrap();
        app.invoke(apply(&FeedsAction::UpsertFetched {
            feeds: Vec::new(),
            articles: vec![Article {
                id: Uuid::from_u128(0xFEED_0011),
                feed_id,
                title: "Fetched".into(),
                url: "https://example.test/fetched".into(),
                summary: String::new(),
                author: String::new(),
                published: 0,
                read: false,
                starred: false,
                tags: Vec::new(),
                blocks: Vec::new(),
            }],
        }))
        .unwrap();
        assert_eq!(app.state().store.articles.len(), 1);

        // Undo takes back the rename, past the fetched article, which stays.
        assert!(app.undo());
        let state = app.state();
        assert_ne!(state.store.feeds[0].title, "Mine");
        assert_eq!(state.store.articles.len(), 1);

        // The subscriptions an import added go in one step.
        let folder = Folder {
            id: Uuid::from_u128(0xFEED_0020),
            name: "Reading".into(),
            order: 0,
            collapsed: false,
        };
        let imported = |n: u128, url: &str| Feed {
            id: Uuid::from_u128(n),
            title: url.into(),
            url: url.into(),
            site_url: url.into(),
            folder: Some(folder.id),
            order: 0,
            kind: FeedKind::Rss,
        };
        app.invoke(apply(&FeedsAction::ImportSubscriptions {
            folders: vec![folder.clone()],
            feeds: vec![
                imported(0xFEED_0021, "https://a.test/rss"),
                imported(0xFEED_0022, "https://b.test/rss"),
            ],
        }))
        .unwrap();
        assert_eq!(app.state().store.feeds.len(), 3);
        assert_eq!(app.state().store.folders.len(), 1);
        assert!(app.undo());
        assert_eq!(app.state().store.feeds.len(), 1);
        assert!(app.state().store.folders.is_empty());
    }
}
