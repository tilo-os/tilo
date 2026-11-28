use std::sync::{Arc, Mutex};
use tilo_app_test_host::TestApp;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, ResourceViewLocator,
};
use tilo_app::framework::harness_device::HarnessDevice;
use tilo_app::framework::testing::TEST_WORKSPACE;
use tilo_app::permissions::{MemberId, Role};
use tilo_app::prelude::*;
use tilo_app::{RemoteActions, RemoteActionsGuard};
use tilo_reference::link::{
    AppId, HostOrigin, Link, LinkSecrets, LinkTarget, Mode, ResourceAddress, ResourceSegment,
    Scope, WebEntries, WebEntry,
};

const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
const ENTRY: &str = "https://tilo.so/";
/// Who the test's notebook belongs to.
const PERSON: &str = "test-author";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Page {
    #[model(label)]
    pub title: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notebook {
    pub pages: Collection<Page>,
    pub tags: Collection<Page>,
}

fn page_views(field: &str, _: &Id) -> Vec<tilo_actions::ResourceViewSpec> {
    match field {
        "pages" => vec![
            tilo_actions::ResourceViewSpec::new("page", "Page", "open")
                .with_primary(true)
                .with_range(tilo_actions::ResourceViewRangeSpec::new("open")),
            tilo_actions::ResourceViewSpec::new("outline", "Outline", "open"),
        ],
        _ => Vec::new(),
    }
}

#[resource(kind = "test.notebook", item_views = page_views, catalogue = "tests/locales")]
impl Notebook {
    /// Add a page.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.pages.push(cx.id(), Page { title });
    }

    /// Add a tag.
    #[action]
    pub fn tag(&mut self, cx: &mut ActionCx, title: String) {
        self.tags.push(cx.id(), Page { title });
    }
}

struct NotebookApp;

impl ResourceApp for NotebookApp {
    type Resource = Notebook;
    const ID: &'static str = "so.tilo.test-notebook";
    const TITLE: &'static str = "Notebook";

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        cx.on_open_item(|_| Ok(()));
        Box::new(Column::new())
    }
}

/// A workspace host answering view lookups with its locator, announcing `entry`.
struct Host {
    entry: Option<String>,
    role: Role,
    asked: Mutex<Vec<CatalogueQuery>>,
}

impl ActionProvider for Host {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let provider = query.provider.clone().unwrap_or_default();
        self.asked.lock().unwrap().push(query);
        Pending::ready(Ok(CataloguePage {
            locator: Some(ResourceViewLocator {
                origin: "https://host.example".into(),
                workspace: WORKSPACE.into(),
                host_device: "ab".repeat(32),
                provider,
                web_entry: self.entry.clone(),
            }),
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotPermitted))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }

    /// What the window shows, decided as the device keeping the notebook's workspace decides it
    /// for a person of `role` there.
    fn shown_actions(
        &self,
        _caller: &Caller,
        ask: tilo_actions::shown::ShownAsk,
    ) -> Pending<Result<Vec<tilo_actions::shown::ActionDecision>, ActionCallError>> {
        let device = HarnessDevice::member_of(TEST_WORKSPACE, &MemberId::person(PERSON), self.role);
        Pending::ready(Ok(device.decide(&ask.actions)))
    }
}

fn serve(entry: Option<&str>) -> (Arc<Host>, RemoteActionsGuard) {
    serve_as(entry, Role::Owner)
}

/// [`serve`] for a person of `role` in the notebook's workspace.
fn serve_as(entry: Option<&str>, role: Role) -> (Arc<Host>, RemoteActionsGuard) {
    let host = Arc::new(Host {
        entry: entry.map(str::to_owned),
        role,
        asked: Mutex::default(),
    });
    tilo_app_test_host::answer_calls_in_process();
    let guard = RemoteActions::enter(host.clone(), Caller::local_person("owner"));
    (host, guard)
}

fn find<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        (resource.id.as_str() == id)
            .then_some(resource)
            .or_else(|| find(&resource.children, id))
    })
}

fn param_names(resource: &tilo_actions::ResourceSpec, action: &str) -> Vec<String> {
    resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .expect("the action is published")
        .params()
        .iter()
        .map(|param| param.name.to_string())
        .collect()
}

fn page_of(app: &mut TestApp<NotebookApp>, title: &str) -> String {
    app.invoke(Notebook::ADD.with(title)).unwrap();
    let state = app.state();
    let id = state
        .pages
        .ids()
        .find(|id| state.pages.get(id).is_some_and(|page| page.title == title))
        .cloned()
        .unwrap();
    tilo_resource::items::item_resource_id::<Notebook>("pages", &id)
}

fn expected(app: &TestApp<NotebookApp>, page: &str, mode: Mode, view: Option<&str>) -> String {
    let span = None;
    Link {
        mode,
        target: LinkTarget {
            scope: Scope::Workspace {
                host: HostOrigin::new("https://host.example").unwrap(),
                id: WORKSPACE.parse().unwrap(),
                slug: None,
            },
            app: Some(AppId::parse(NotebookApp::ID).unwrap()),
            record: Some(ResourceSegment::new(app.record().as_str()).unwrap()),
            resource: Some(ResourceAddress {
                kind: "test.notebook.pages".into(),
                id: page.to_owned(),
                span,
            }),
            view: view.map(str::to_owned),
            discuss: None,
        },
        secrets: LinkSecrets::default(),
    }
    .to_url(&WebEntry::new(ENTRY).unwrap())
    .unwrap()
}

tilo_test_env::scenario! {
    fn share_links_are_asked_of_the_host_and_refused_with_a_reason;

    phase a_resource_with_a_view_publishes_share_link_with_range_params_only_for_a_range;
    {
        let (_host, _guard) = serve(Some(ENTRY));
        let mut app = TestApp::<NotebookApp>::new().unwrap();
        let page = page_of(&mut app, "Intro");
        app.invoke(Notebook::TAG.with("draft")).unwrap();

        let resources = app.harness().resources();
        let published = find(&resources, &page).expect("the page is published");
        assert_eq!(
            param_names(published, "share-link"),
            ["view", "start", "end", "unit", "mode"]
        );
        assert_eq!(
            param_names(published, "share-invite-link"),
            ["view", "start", "end", "unit", "mode", "role", "expires_in"]
        );
        // A tag declares no view of its own, but the view opens it as an
        // item: the framework's item view is a view, so it too has a link.
        let tag_id = app.state().tags.ids().next().cloned().unwrap();
        let tag = tilo_resource::items::item_resource_id::<Notebook>("tags", &tag_id);
        let tag = find(&resources, &tag).expect("the tag is published");
        assert_eq!(
            tag.views
                .iter()
                .map(|view| view.id.as_str())
                .collect::<Vec<_>>(),
            ["item"]
        );
        assert_eq!(param_names(tag, "share-link"), ["view", "mode"]);
    }

    phase an_items_link_names_the_item_it_opens_not_its_record;
    // The item view the app's own opener gives an item is also what a link
    // opens: the link names the item, so whoever opens it lands on the item
    // rather than on the record it is kept in.
    {
        let (_host, _guard) = serve(Some(ENTRY));
        let mut app = TestApp::<NotebookApp>::new().unwrap();
        app.invoke(Notebook::TAG.with("draft")).unwrap();
        let tag_id = app.state().tags.ids().next().cloned().unwrap();
        let tag = tilo_resource::items::item_resource_id::<Notebook>("tags", &tag_id);

        let answer = app
            .call_on(&tag, "share-link", serde_json::json!({}))
            .unwrap()
            .unwrap();
        let url = answer["url"].as_str().expect("a link");
        let entries = WebEntries::new(vec![WebEntry::new(ENTRY).unwrap()]);
        let (link, damaged) = Link::parse_or_workspace(url, &entries).unwrap();
        assert!(damaged.is_none(), "{url}");
        assert_eq!(link.target.app, Some(AppId::parse(NotebookApp::ID).unwrap()));
        assert_eq!(
            link.target.resource.as_ref().map(|resource| resource.id.as_str()),
            Some(tag.as_str()),
            "the link names the tag, not the notebook"
        );
        assert_eq!(link.target.view, None, "the item view is the primary view");
    }

    phase share_link_asks_the_host_and_renders_the_link_under_its_entry;
    {
        let (host, _guard) = serve(Some(ENTRY));
        let mut app = TestApp::<NotebookApp>::new().unwrap();
        let page = page_of(&mut app, "Intro");

        let answer = app
            .call_on(&page, "share-link", serde_json::json!({}))
            .unwrap()
            .unwrap();
        assert_eq!(answer["url"], expected(&app, &page, Mode::App, None));
        let asked = host.asked.lock().unwrap().clone();
        let query = asked.last().unwrap();
        assert_eq!(
            query.view_resource.as_ref().map(|view| view.id.as_str()),
            Some(page.as_str())
        );
        assert_eq!(
            query.provider.as_deref(),
            Some(NotebookApp::ID)
        );

        let answer = app
            .call_on(
                &page,
                "share-link",
                serde_json::json!({"view": "outline", "mode": "desktop"}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            answer["url"],
            expected(&app, &page, Mode::Desktop, Some("outline"))
        );

        let ranged = app
            .call_on(
                &page,
                "share-link",
                serde_json::json!({"start": 2, "end": 5}),
            )
            .unwrap()
            .unwrap();
        assert!(
            ranged["url"].as_str().unwrap().ends_with("?chars=2-5"),
            "{ranged}"
        );
        assert!(
            app.call_on(
                &page,
                "share-link",
                serde_json::json!({"view": "outline", "start": 2, "end": 5}),
            )
            .is_err(),
            "the outline opens at no range"
        );
    }

    phase a_host_without_a_web_entry_refuses_with_its_reason;
    {
        let (_host, _guard) = serve(None);
        let mut app = TestApp::<NotebookApp>::new().unwrap();
        let page = page_of(&mut app, "Intro");
        let refused = app
            .call_on(&page, "share-link", serde_json::json!({}))
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("This workspace has no server that serves links"),
            "{refused}"
        );
    }

    phase share_invite_link_is_refused_without_the_invite_grant;
    {
        let (_host, _guard) = serve_as(Some(ENTRY), Role::Editor);
        let mut app = TestApp::<NotebookApp>::with_role(Role::Editor).unwrap();
        let page = page_of(&mut app, "Intro");
        let refused = app
            .call_confirmed_on(&page, "share-invite-link", serde_json::json!({}))
            .unwrap_err();
        assert!(refused.to_string().contains("may not invite"), "{refused}");
        // A viewer may share a plain link, which grants nothing.
        app.set_role(Role::Viewer);
        assert!(
            app.call_on(&page, "share-link", serde_json::json!({}))
                .is_ok()
        );
    }
}
