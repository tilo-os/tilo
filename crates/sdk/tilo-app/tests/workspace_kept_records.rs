//! A record one app keeps for every app of the workspace
//! (`collab_types::WORKSPACE_DOCUMENTS`): one record in the workspace for
//! every member, not one per person.

use tilo_app::Role;
use tilo_app::prelude::*;

/// The app keeping the record: Browser, whose bookmarks the workspace keeps.
const BROWSER: &str = "so.tilo.browser";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub items: Vec<String>,
}

#[resource(
    kind = "test.shelf",
    app = "so.tilo.browser",
    catalogue = "tests/locales"
)]
impl Shelf {}

/// What the workspace bookmarked.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bookmarks {
    pub pages: Collection<String>,
}

#[resource(
    kind = "browser.personal-bookmarks",
    app = "so.tilo.browser",
    catalogue = "tests/locales"
)]
impl Bookmarks {
    /// Keep a page.
    #[action]
    pub fn keep(&mut self, cx: &mut ActionCx, page: String) {
        self.pages.push(cx.id(), page);
    }
}

impl tilo_app::tilo_resource::SharedResource for Bookmarks {
    const OWNER: &'static str = BROWSER;
}

struct BrowserApp;

impl ResourceApp for BrowserApp {
    type Resource = Shelf;
    const ID: &'static str = BROWSER;
    const TITLE: &'static str = "Browser";
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::shared::<Bookmarks>()];

    fn view(_cx: &mut ViewCx<Shelf>) -> Element {
        Box::new(Column::new())
    }
}

fn ada() -> String {
    "ad".repeat(32)
}

fn bob() -> String {
    "b0".repeat(32)
}

fn pages(record: &WorkspaceResource<Bookmarks>) -> Vec<String> {
    let mut pages: Vec<String> = record.with(|state| state.pages.values().cloned().collect());
    pages.sort();
    pages
}

tilo_test_env::scenario! {
    fn a_record_the_workspace_keeps_is_one_record_for_every_member;

    phase two_members_open_the_same_record_of_the_workspace;
    {
        let mut adas = TestApp::<BrowserApp>::for_person(&ada()).unwrap();
        let mut bobs = TestApp::<BrowserApp>::for_person_with_role(&bob(), Role::Editor).unwrap();
        let on_adas = adas.shared_workspace::<Bookmarks>(BROWSER).unwrap();
        let on_bobs = bobs.shared_workspace::<Bookmarks>(BROWSER).unwrap();
        let route = on_adas.record().route().expect("the workspace's record syncs");
        assert_eq!(route, on_bobs.record().route().unwrap(), "one record for both of them");
        assert_eq!(route.app_id, BROWSER);
        assert_eq!(
            route.cluster_id.to_string(),
            tilo_app::framework::headless::TEST_WORKSPACE,
            "it is the workspace's, not a person's own store's"
        );
        assert_eq!(
            route.document.as_deref(),
            Some(collab_types::workspace_document_id(Bookmarks::KIND).as_str())
        );

        on_adas.invoke(&Bookmarks::KEEP.with("from-ada")).unwrap();
        on_bobs.invoke(&Bookmarks::KEEP.with("from-bob")).unwrap();
        on_bobs
            .record()
            .merge_peer_document(on_adas.record().document().unwrap())
            .unwrap();
        on_adas
            .record()
            .merge_peer_document(on_bobs.record().document().unwrap())
            .unwrap();
        assert_eq!(pages(&on_adas), ["from-ada", "from-bob"]);
        assert_eq!(pages(&on_adas), pages(&on_bobs), "each sees what the other kept");
    }

    phase the_record_is_published_as_the_workspaces;
    {
        let report = tilo_app::framework::headless::surface_report::<BrowserApp>();
        let declared = report["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .find(|resource| resource["kind"] == Bookmarks::KIND)
            .cloned()
            .expect("the app declares the record");
        assert_ne!(
            declared["person"],
            serde_json::json!(true),
            "the workspace keeps it, so it is not the person's own"
        );
    }
}

/// Browser as a window that reads the workspace's bookmarks once, when its view is built.
struct ReadsOnce;

impl ResourceApp for ReadsOnce {
    type Resource = Shelf;
    const ID: &'static str = BROWSER;
    const TITLE: &'static str = "Browser";
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::shared::<Bookmarks>()];

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        let said = match cx.shared_workspace::<Bookmarks>(BROWSER) {
            Some(bookmarks) if pages(&bookmarks).is_empty() => "nothing bookmarked".to_owned(),
            Some(bookmarks) => pages(&bookmarks).join(", "),
            None => "the bookmarks are not here".to_owned(),
        };
        Box::new(Column::new().push(Box::new(Text::new(&said))))
    }
}

/// Whether `app`'s window says `text`, to a screen reader or in what it draws.
fn says(app: &mut TestApp<ReadsOnce>, text: &str) -> bool {
    let read = app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
    });
    read || app
        .harness()
        .inspection_json()
        .is_ok_and(|drawn| drawn.contains(text))
}

/// A window opened on a device the workspace's record has not reached yet, another member
/// having made it, waits for it before it shows its view, and shows what arrived: a view built
/// once while the record was on its way would go on reading that the workspace has none, and
/// what the person then kept would stay on this device.
#[test]
fn a_window_shows_the_workspaces_record_once_it_arrived() {
    let mut app = TestApp::<ReadsOnce>::awaiting_the_workspace(
        tilo_app::framework::headless::TEST_WORKSPACE,
        BROWSER,
    )
    .unwrap();
    app.frame();
    assert!(
        !says(&mut app, "the bookmarks are not here"),
        "on their way, the bookmarks are not read as missing"
    );
    assert!(!says(&mut app, "nothing bookmarked"), "nor as none");

    app.workspace_delivers::<Bookmarks>(BROWSER, |bookmarks| {
        bookmarks
            .invoke(&Bookmarks::KEEP.with("from-bob"))
            .map_err(|error| error.to_string())
    })
    .unwrap();
    for _ in 0..4 {
        app.frame();
    }
    assert!(
        says(&mut app, "from-bob"),
        "the window shows what the other member kept, with no reload"
    );
}
